//! The outbound relay connector: a heartbeat that keeps device enrollments, revocations and iroh
//! bindings at the relay, and hands encrypted pushes to APNs through it. Present only when
//! `service.toml` names a relay.
use super::{
    Device, EnrollmentDecision, PushDelivery, Service, Transport,
    decoded, now_ms, registration_ended, relay_error_code, without_relay_extensions,
};
use crate::push::local_minutes;
use alondra_e2e::sign_http_challenge;
use anyhow::{Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Method, Url};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// The connector checks devices, revocations and bindings this often.
#[cfg(not(test))]
const HEARTBEAT: Duration = Duration::from_secs(60);
#[cfg(test)]
const HEARTBEAT: Duration = Duration::from_millis(50);
/// After a failed check the next one comes this soon.
#[cfg(not(test))]
const RETRY: Duration = Duration::from_secs(3);
#[cfg(test)]
const RETRY: Duration = Duration::from_millis(5);

pub(crate) struct RelayConnector {
    url: Url,
    owner_sub: String,
    resync: AtomicBool,
    /// The relay epoch the iroh endpoint was last bound in, and when a failed attempt was.
    iroh_bound: std::sync::Mutex<Option<([u8; 16], Option<std::time::Instant>)>>,
    /// The device endpoints bound on this Mac's behalf in that epoch, and when one last failed.
    devices_bound: std::sync::Mutex<DevicesBound>,
    wake: tokio::sync::Notify,
}

#[derive(Default)]
struct DevicesBound {
    epoch: Option<[u8; 16]>,
    nodes: std::collections::BTreeMap<String, String>,
    failed: Option<std::time::Instant>,
}

#[derive(Deserialize)]
struct Bootstrap {
    epoch: String,
    #[serde(default)]
    iroh_relay: Option<String>,
}

/// The iroh relay admits this Mac's endpoint only once the Alondra relay holds the binding,
/// proven by the endpoint key over this domain and the Mac ID.
const IROH_NODE_PROOF_DOMAIN: &[u8] = b"alondra-iroh-node-v1\0";
/// A refused or failed binding is tried again after this long within the same relay epoch.
const IROH_BIND_RETRY: Duration = Duration::from_secs(300);

pub(super) fn checked_origin(url: &str) -> Result<Url> {
    let relay = Url::parse(url)?;
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
    Ok(relay)
}

impl RelayConnector {
    pub(super) fn new(url: Url, owner_sub: String) -> Self {
        Self {
            url, owner_sub, resync: AtomicBool::new(false), iroh_bound: std::sync::Mutex::new(None),
            devices_bound: std::sync::Mutex::default(), wake: tokio::sync::Notify::new(),
        }
    }

    /// The device list changed: re-enroll and re-sync revocations on the next iteration.
    pub(super) fn resync(&self) {
        self.resync.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// A device said hello from a new iroh endpoint: bind it at the relay on the next iteration.
    pub(super) fn bind_devices_soon(&self) {
        self.devices_bound.lock().expect("device binding lock poisoned").failed = None;
        self.wake.notify_one();
    }

    #[cfg(test)]
    pub(super) fn resync_pending(&self) -> bool {
        self.resync.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn set_url(&mut self, url: Url) {
        self.url = url;
    }
}

impl Service {
    fn relay(&self) -> Result<&RelayConnector> {
        self.relay.as_ref().ok_or_else(|| anyhow!("this Mac has no relay configured"))
    }

    #[cfg(test)]
    pub(super) fn relay_url(&self) -> Url {
        self.relay.as_ref().expect("relay configured").url.clone()
    }

    #[cfg(test)]
    pub(super) fn set_relay_url(&mut self, url: Url) {
        match &mut self.relay {
            Some(relay) => relay.set_url(url),
            None => self.relay = Some(RelayConnector::new(url, "owner".into())),
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        Ok(self.relay()?.url.join(path.trim_start_matches('/'))?)
    }

    pub(super) async fn bootstrap(&self) -> Result<[u8; 16]> {
        let response = self
            .client
            .get(self.endpoint("/v1/bootstrap")?)
            .send()
            .await?
            .error_for_status()?;
        let boot: Bootstrap = response.json().await?;
        self.announce_iroh_relay(boot.iroh_relay);
        decoded(&boot.epoch)?
            .try_into()
            .map_err(|_| anyhow!("invalid relay epoch"))
    }

    pub(super) async fn signed(
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

    pub(super) async fn enroll(&self, epoch: &[u8; 16]) -> Result<()> {
        let owner_sub = self.relay()?.owner_sub.clone();
        for (id, device) in &self.devices.snapshot() {
            if !self.auth.approved_key_matches(id, &device.signing) {
                continue;
            }
            // Record one attempt before any network write. A relay-side revocation must
            // survive retries and restarts; a new approval needs a new device ID.
            let (owned, key) = (id.clone(), device.signing.to_sec1_bytes());
            match self
                .journal
                .off_runtime(move |journal| journal.reserve_enrollment(&owned, &key))
                .await?
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
                &json!({"device_id":id,"owner_sub":owner_sub,"signing_key":device.signing_wire}),
            )?;
            self.signed(Method::POST, "/v1/devices", body, epoch)
                .await?
                .error_for_status()?;
            let owned = id.clone();
            if self.auth.approved_key_matches(id, &device.signing) {
                self.journal
                    .off_runtime(move |journal| journal.activate_enrollment(&owned))
                    .await?;
            } else {
                self.journal
                    .off_runtime(move |journal| journal.revoke_enrollment(&owned))
                    .await?;
                let path = format!("/v1/devices/{id}");
                self.signed(Method::DELETE, &path, Vec::new(), epoch)
                    .await?
                    .error_for_status()?;
            }
        }
        Ok(())
    }

    /// Binds this Mac's iroh endpoint at the relay once per relay epoch, so the iroh relay lets
    /// it in. A relay without iroh support refuses it; work goes on and it is tried again later.
    pub(super) async fn keep_iroh_endpoint_bound(&self, epoch: &[u8; 16]) {
        let Ok(relay) = self.relay() else { return };
        if !self.iroh_config().enabled {
            return;
        }
        let due = match *relay.iroh_bound.lock().expect("iroh binding lock poisoned") {
            Some((bound, None)) => bound != *epoch,
            Some((bound, Some(failed))) => bound != *epoch || failed.elapsed() >= IROH_BIND_RETRY,
            None => true,
        };
        if !due {
            return;
        }
        let failed = match self.bind_iroh_endpoint(epoch).await {
            Ok(()) => {
                log!("iroh endpoint {} bound at the relay", self.iroh_key().public());
                None
            }
            Err(error) => {
                log!("iroh endpoint not bound at the relay: {error:#}");
                Some(std::time::Instant::now())
            }
        };
        *relay.iroh_bound.lock().expect("iroh binding lock poisoned") = Some((*epoch, failed));
    }

    async fn bind_iroh_endpoint(&self, epoch: &[u8; 16]) -> Result<()> {
        let key = self.iroh_key();
        let proof = key.sign(&[IROH_NODE_PROOF_DOMAIN, self.mac_id.as_bytes()].concat());
        let body = serde_json::to_vec(&json!({
            "iroh_node_id": key.public().to_string(),
            "proof": URL_SAFE_NO_PAD.encode(proof.to_bytes()),
        }))?;
        self.signed(Method::PUT, "/v1/iroh-node", body, epoch).await?.error_for_status()?;
        Ok(())
    }

    /// Binds, at the relay, the iroh endpoint each device last said hello from, once per relay
    /// epoch and whenever one changes. A device with no account (a terminal paired directly)
    /// reaches the iroh relay only this way. Needs this Mac's own endpoint bound first.
    pub(super) async fn keep_device_iroh_nodes_bound(&self, epoch: &[u8; 16]) {
        let Ok(relay) = self.relay() else { return };
        let mac_bound = matches!(*relay.iroh_bound.lock().expect("iroh binding lock poisoned"), Some((bound, None)) if bound == *epoch);
        if !self.iroh_config().enabled || !mac_bound {
            return;
        }
        let pending: Vec<_> = {
            let mut bound = relay.devices_bound.lock().expect("device binding lock poisoned");
            if bound.epoch != Some(*epoch) {
                *bound = DevicesBound { epoch: Some(*epoch), ..DevicesBound::default() };
            }
            if bound.failed.is_some_and(|failed| failed.elapsed() < IROH_BIND_RETRY) {
                return;
            }
            self.iroh_hellos()
                .into_iter()
                .filter(|(id, hello)| bound.nodes.get(id) != Some(&hello.node))
                .collect()
        };
        for (id, hello) in pending {
            let result = async {
                let body = serde_json::to_vec(&json!({"iroh_node_id": hello.node, "hello_signature": hello.signature}))?;
                let path = format!("/v1/devices/{id}/iroh-node");
                self.signed(Method::PUT, &path, body, epoch).await?.error_for_status()?;
                anyhow::Ok(())
            }
            .await;
            let mut bound = relay.devices_bound.lock().expect("device binding lock poisoned");
            match result {
                Ok(()) => {
                    log!("iroh endpoint of {id} bound at the relay");
                    bound.nodes.insert(id, hello.node);
                }
                Err(error) => {
                    log!("iroh endpoint of {id} not bound at the relay: {error:#}");
                    bound.failed = Some(std::time::Instant::now());
                }
            }
        }
    }

    pub(super) async fn sync_revocations(
        &self,
        epoch: &[u8; 16],
        synced: &mut HashSet<String>,
    ) -> Result<()> {
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
            let owned = id.clone();
            self.journal
                .off_runtime(move |journal| journal.revoke_enrollment(&owned))
                .await?;
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
        let Ok(relay) = self.relay() else { return };
        let mut enrolled_epoch: Option<[u8; 16]> = None;
        let mut revoked_this_epoch = HashSet::new();
        loop {
            let wait = match self.keep_registered(&mut enrolled_epoch, &mut revoked_this_epoch).await {
                Ok(()) => HEARTBEAT,
                Err(error) => {
                    log!("service connector: {error:#}");
                    RETRY
                }
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = relay.wake.notified() => {}
            }
        }
    }

    /// One check: a new relay epoch (a restarted relay) re-enrolls every approved device, and
    /// any failure forces a fresh epoch next time.
    pub(super) async fn keep_registered(
        &self,
        enrolled_epoch: &mut Option<[u8; 16]>,
        revoked_this_epoch: &mut HashSet<String>,
    ) -> Result<()> {
        let relay = self.relay()?;
        if relay.resync.swap(false, Ordering::SeqCst) {
            *enrolled_epoch = None;
        }
        let result = async {
            let epoch = self.bootstrap().await?;
            if *enrolled_epoch != Some(epoch) {
                revoked_this_epoch.clear();
            }
            self.sync_revocations(&epoch, revoked_this_epoch).await?;
            if *enrolled_epoch != Some(epoch) {
                self.enroll(&epoch).await?;
                *enrolled_epoch = Some(epoch);
            }
            self.keep_iroh_endpoint_bound(&epoch).await;
            self.keep_device_iroh_nodes_bound(&epoch).await;
            Ok(())
        }
        .await;
        if result.is_err() {
            *enrolled_epoch = None;
        }
        result
    }

    pub(super) async fn send_device_push(
        &self,
        id: &str,
        device: &Device,
        binding: &str,
        payload: &Value,
        with_sound: bool,
        delivery: PushDelivery,
        epoch: &mut Option<[u8; 16]>,
    ) -> Result<()> {
        self.require_device_approval(id, device, Transport::Relay)?;
        let (payload, with_sound) = match delivery {
            PushDelivery::Alert => self.journal.quiet_hours(id)?.apply(payload, with_sound, local_minutes()),
            PushDelivery::Background | PushDelivery::Badge => (payload.clone(), false),
        };
        let mut body = self.encode_push_or_minimal(id, device, binding, &payload, with_sound, delivery)?;
        let current_epoch = match *epoch {
            Some(epoch) => epoch,
            None => {
                let fresh = self.bootstrap().await?;
                *epoch = Some(fresh);
                fresh
            }
        };
        self.require_device_approval(id, device, Transport::Relay)?;
        if self.journal.push_binding(id)?.as_deref() != Some(binding) {
            bail!("device push binding changed while preparing delivery");
        }
        loop {
            let response = self
                .signed(Method::POST, "/v1/mac/push", body.clone(), &current_epoch)
                .await?;
            let status = response.status();
            if status == reqwest::StatusCode::ACCEPTED {
                self.auth.usage().push_ok(id, alondra_core::now_millis());
                return Ok(());
            }
            let code = relay_error_code(&response.bytes().await.unwrap_or_default());
            if status == reqwest::StatusCode::NOT_FOUND && code == "push_registration_not_found" {
                self.auth.usage().push_not_found(id, alondra_core::now_millis());
            }
            if registration_ended(status, &code) {
                self.journal.forget_push_binding(id, binding)?;
                bail!("the relay no longer has a push registration for this device (HTTP {status}, error {code}); stopped pushing to it until it registers again");
            }
            if status == reqwest::StatusCode::BAD_REQUEST && code == "invalid_push" {
                match without_relay_extensions(&body)? {
                    Some(older) if delivery == PushDelivery::Alert => {
                        body = older;
                        continue;
                    }
                    Some(_) if delivery == PushDelivery::Background => bail!("the relay predates background pushes: deploy the current relay so answered cards leave the phone"),
                    Some(_) => bail!("the relay predates badge pushes: deploy the current relay so the icon badge drops while the app is closed"),
                    None => {}
                }
            }
            bail!("relay did not accept encrypted push (HTTP {status}, error {code})");
        }
    }

    /// Hands one Live Activity report to the relay: what it answered, for the caller to decide
    /// when to send again. A report is never queued: the next one carries the state of then.
    pub(crate) async fn send_device_activity(
        &self,
        id: &str,
        binding: &str,
        payload: &Value,
        flags: crate::activity_push::Flags,
        epoch: &mut Option<[u8; 16]>,
    ) -> Result<(reqwest::StatusCode, String)> {
        let device = self.devices.get(id).ok_or_else(|| anyhow!("device is not configured for the service"))?;
        self.require_device_approval(id, &device, Transport::Relay)?;
        let body = super::seal_activity(&self.mac_id, &self.signing, id, &device.agreement, binding, payload, flags)?;
        let current_epoch = match *epoch {
            Some(epoch) => epoch,
            None => *epoch.insert(self.bootstrap().await?),
        };
        let response = self.signed(Method::POST, "/v1/mac/activity", body, &current_epoch).await?;
        let status = response.status();
        let code = if status.is_success() { String::new() } else { relay_error_code(&response.bytes().await.unwrap_or_default()) };
        Ok((status, code))
    }
}
