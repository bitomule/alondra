//! The outbound relay connector: enrolls approved devices with the relay, long-polls it for
//! envelopes, acknowledges and delivers responses, and hands encrypted pushes to APNs through it.
//! Present only when `service.toml` names a relay.
use super::{
    Device, EnrollmentDecision, PreparedWork, PushDelivery, Service, Transport, Work,
    approval_bound_delivery, decoded, now_ms, relay_error_code, without_relay_extensions,
};
use crate::push::local_minutes;
use alondra_e2e::sign_http_challenge;
use anyhow::{Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Method, StatusCode, Url};
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

pub(crate) struct RelayConnector {
    url: Url,
    owner_sub: String,
    resync: AtomicBool,
    wake: tokio::sync::Notify,
}

#[derive(Deserialize)]
struct Bootstrap {
    epoch: String,
}

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
        Self { url, owner_sub, resync: AtomicBool::new(false), wake: tokio::sync::Notify::new() }
    }

    /// The device list changed: re-enroll and re-sync revocations on the next iteration.
    pub(super) fn resync(&self) {
        self.resync.store(true, Ordering::SeqCst);
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
        let mut changes = tokio::task::JoinSet::new();
        let mut enrolled_epoch: Option<[u8; 16]> = None;
        let mut revoked_this_epoch = HashSet::new();
        loop {
            while let Some(result) = changes.try_join_next() {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => log!("service changes: {error:#}"),
                    Err(error) => log!("service changes task: {error}"),
                }
            }
            let result = self
                .poll_work(&mut enrolled_epoch, &mut revoked_this_epoch, &mut changes)
                .await;
            if let Err(e) = result {
                log!("service connector: {e:#}");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }

    /// One bounded iteration; any failure forces a fresh epoch next time. POSTs
    /// are never replayed here; work redelivery keeps its durable journal rules.
    pub(super) async fn poll_work(
        self: &Arc<Self>,
        enrolled_epoch: &mut Option<[u8; 16]>,
        revoked_this_epoch: &mut HashSet<String>,
        changes: &mut tokio::task::JoinSet<Result<()>>,
    ) -> Result<()> {
        let relay = self.relay()?;
        if relay.resync.swap(false, Ordering::SeqCst) {
            *enrolled_epoch = None;
        }
        let result = async {
                let epoch = match *enrolled_epoch {
                    Some(epoch) => epoch,
                    None => self.bootstrap().await?,
                };
                if *enrolled_epoch != Some(epoch) {
                    revoked_this_epoch.clear();
                }
                self.sync_revocations(&epoch, revoked_this_epoch)
                    .await?;
                if *enrolled_epoch != Some(epoch) {
                    self.enroll(&epoch).await?;
                    *enrolled_epoch = Some(epoch);
                }
                let path = "/v1/work?wait_s=25";
                let response = tokio::select! {
                    response = self.signed(Method::GET, path, Vec::new(), &epoch) => response?.error_for_status()?,
                    () = relay.wake.notified() => return Ok(()),
                };
                if response.status() == StatusCode::NO_CONTENT {
                    return Ok::<_, anyhow::Error>(());
                }
                let work: Work = response.json().await?;
                self.schedule_work(work, epoch, changes).await
        }.await;
        if result.is_err() {
            *enrolled_epoch = None;
        }
        result
    }

    async fn drain_ack(response: reqwest::Response) -> Result<()> {
        let mut response = response.error_for_status()?;
        let mut bytes = 0usize;
        while let Some(chunk) = response.chunk().await? {
            bytes += chunk.len();
            if bytes > 4096 {
                bail!("relay acknowledgement too large");
            }
        }
        Ok(())
    }

    /// Only validated changes reads leave the serial work loop. The guard is
    /// acquired synchronously before spawning, including for lease redelivery.
    pub(super) async fn schedule_work(
        self: &Arc<Self>,
        work: Work,
        epoch: [u8; 16],
        changes: &mut tokio::task::JoinSet<Result<()>>,
    ) -> Result<()> {
        let Some(prepared) = self.prepare_work(&work, Transport::Relay)? else {
            return Ok(());
        };
        if prepared.guard.changes {
            let service = self.clone();
            changes.spawn(async move { service.handle_prepared(work, &epoch, prepared).await });
            Ok(())
        } else {
            self.handle_prepared(work, &epoch, prepared).await
        }
    }

    #[cfg(test)]
    pub(super) async fn handle(&self, work: Work, epoch: &[u8; 16]) -> Result<()> {
        let Some(prepared) = self.prepare_work(&work, Transport::Relay)? else {
            return Ok(());
        };
        self.handle_prepared(work, epoch, prepared).await
    }

    /// Acknowledges the receipt to the relay once the journal holds it, executes, and uploads
    /// the sealed response while the approval lease keeps a revocation from finishing mid-delivery.
    pub(super) async fn handle_prepared(
        &self,
        work: Work,
        epoch: &[u8; 16],
        prepared: PreparedWork,
    ) -> Result<()> {
        let e = &work.envelope;
        let receipt_path = format!("/v1/work/{}/receipt", e.request_id);
        let (response, device, _guard) = self
            .execute(e, prepared, Transport::Relay, || async {
                Self::drain_ack(
                    self.signed(Method::POST, &receipt_path, Vec::new(), epoch)
                        .await?,
                )
                .await
            })
            .await?;
        let response_path = format!("/v1/work/{}/response", e.request_id);
        approval_bound_delivery(
            &self.home,
            || self.require_device_approval(&e.sender, &device, Transport::Relay),
            || async {
                Self::drain_ack(
                    self.signed(Method::POST, &response_path, response, epoch)
                        .await?,
                )
                .await
            },
        )
        .await?;
        Ok(())
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
                return Ok(());
            }
            let code = relay_error_code(&response.bytes().await.unwrap_or_default());
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
}
