//! Disposable system-test client. Uses real OIDC, signed HTTP and encrypted envelopes.
use alondra_e2e::{Context, EnvelopeV1, sign_http_challenge};
use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::{SecretKey, ecdsa::SigningKey, elliptic_curve::sec1::ToSec1Point};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MACS: [&str; 3] = ["system-mac-a1", "system-mac-a2", "system-mac-b1"];
const LINKS: [(&str, usize); 5] = [
    ("system-one-a1", 0),
    ("system-one-a2", 1),
    ("system-two-a1", 0),
    ("system-two-a2", 1),
    ("system-other-b1", 2),
];
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn key(code: u8) -> SigningKey {
    let mut bytes = [code; 32];
    if code == 1 {
        bytes = [0; 32];
        bytes[31] = 1;
    }
    SigningKey::from_slice(&bytes).unwrap()
}
fn agreement(code: u8) -> SecretKey {
    SecretKey::from_slice(&[code; 32]).unwrap()
}
fn public(code: u8) -> String {
    B64.encode(key(code).verifying_key().to_sec1_point(false).as_bytes())
}
fn owner(mac: usize) -> usize {
    usize::from(mac == 2)
}
fn save(name: &str, value: Value) -> Result<()> {
    let target = Path::new("/system").join(name);
    let temporary = target.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    std::fs::rename(temporary, target)?;
    Ok(())
}
#[derive(Clone)]
struct Driver {
    http: reqwest::Client,
    fixture: Value,
    tokens: Vec<String>,
    refresh_tokens: Vec<String>,
    epoch: [u8; 16],
}
impl Driver {
    async fn open() -> Result<Self> {
        let fixture: Value = serde_json::from_slice(&std::fs::read("/system/fixture.json")?)?;
        ensure!(
            fixture["marker"] == "alondra-disposable-system-v1"
                && fixture["relay"] == "http://relay:18081"
                && fixture["keycloak"] == "http://keycloak:8080",
            "only disposable service endpoints allowed"
        );
        let realm = fixture["realm"].as_str().context("realm")?;
        ensure!(realm.starts_with("system-"), "non-test realm refused");
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        let mut tokens = vec![];
        let mut refresh_tokens = vec![];
        for user in fixture["owners"].as_array().context("owners")? {
            ensure!(
                user["username"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("system-owner-")),
                "non-test credentials refused"
            );
            let response = http
                .post(format!(
                    "http://keycloak:8080/realms/{realm}/protocol/openid-connect/token"
                ))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(format!(
                    "grant_type=password&client_id=system-client&username={}&password={}",
                    user["username"].as_str().unwrap(),
                    user["password"].as_str().context("password")?
                ))
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await?;
            refresh_tokens.push(
                response["refresh_token"]
                    .as_str()
                    .context("refresh token")?
                    .to_owned(),
            );
            tokens.push(
                response["access_token"]
                    .as_str()
                    .context("access token")?
                    .into(),
            );
        }
        let bootstrap = http
            .get("http://relay:18081/v1/bootstrap")
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let epoch = B64
            .decode(bootstrap["epoch"].as_str().context("epoch")?)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("epoch size"))?;
        Ok(Self {
            http,
            fixture,
            tokens,
            refresh_tokens,
            epoch,
        })
    }
    async fn refresh(&self) -> Result<Self> {
        let mut refreshed = self.clone();
        let realm = self.fixture["realm"].as_str().context("realm")?;
        for (who, token) in self.refresh_tokens.iter().enumerate() {
            let reply = self
                .http
                .post(format!(
                    "http://keycloak:8080/realms/{realm}/protocol/openid-connect/token"
                ))
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", "system-client"),
                    ("refresh_token", token),
                ])
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await?;
            refreshed.tokens[who] = reply["access_token"]
                .as_str()
                .context("access token")?
                .into();
            refreshed.refresh_tokens[who] = reply["refresh_token"]
                .as_str()
                .context("refresh token")?
                .into();
        }
        Ok(refreshed)
    }
    async fn call_epoch(
        &self,
        epoch: &[u8; 16],
        code: u8,
        id: &str,
        who: Option<usize>,
        method: &str,
        path: &str,
        body: Vec<u8>,
    ) -> Result<(u16, Value)> {
        self.call_signed(epoch, &key(code), id, who, method, path, body)
            .await
    }
    async fn call_signed(
        &self,
        epoch: &[u8; 16],
        signing: &SigningKey,
        id: &str,
        who: Option<usize>,
        method: &str,
        path: &str,
        body: Vec<u8>,
    ) -> Result<(u16, Value)> {
        ensure!(
            path.starts_with("/v1/") && !path.contains("#"),
            "invalid test path"
        );
        let mut nonce = [0; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| anyhow::anyhow!("nonce"))?;
        let timestamp = now();
        let signature = sign_http_challenge(
            signing,
            id,
            method,
            path,
            epoch,
            &body,
            timestamp,
            &nonce,
        )?;
        let mut request = self
            .http
            .request(
                reqwest::Method::from_bytes(method.as_bytes())?,
                format!("http://relay:18081{path}"),
            )
            .header("x-device-id", id)
            .header("x-relay-epoch", B64.encode(epoch))
            .header("x-device-timestamp-ms", timestamp.to_string())
            .header("x-device-nonce", B64.encode(nonce))
            .header("x-device-signature", signature)
            .body(body);
        if let Some(who) = who {
            request = request.bearer_auth(&self.tokens[who]);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let bytes = response.bytes().await?;
        Ok((
            status,
            if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes)?
            },
        ))
    }
    async fn call(
        &self,
        code: u8,
        id: &str,
        who: Option<usize>,
        method: &str,
        path: &str,
        body: Vec<u8>,
        expected: u16,
    ) -> Result<Value> {
        let (status, value) = self
            .call_epoch(&self.epoch, code, id, who, method, path, body)
            .await?;
        ensure!(
            status == expected,
            "{method} {path}: expected {expected}, received {status}"
        );
        Ok(value)
    }
    fn envelope(&self, link: usize, mac: usize, id: &str) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&EnvelopeV1::seal_random(
            Context {
                route: MACS[mac],
                sender: LINKS[link].0,
                recipient: MACS[mac],
                request_id: id,
                kind: "request",
                expires_at_ms: now() + 120_000,
            },
            br#"{"method":"GET","path":"/v1/tasks"}"#,
            &key(11 + link as u8),
            &agreement(21 + mac as u8).public_key(),
        )?)?)
    }
    async fn submit(&self, link: usize, body: Vec<u8>, expected: u16) -> Result<Value> {
        self.call(
            11 + link as u8,
            LINKS[link].0,
            Some(owner(LINKS[link].1)),
            "POST",
            "/v1/requests",
            body,
            expected,
        )
        .await
    }
    async fn complete(&self, link: usize, id: &str) -> Result<()> {
        let mac = LINKS[link].1;
        let code = 1 + mac as u8;
        let work = self
            .call(
                code,
                MACS[mac],
                None,
                "GET",
                "/v1/work?wait_s=0",
                vec![],
                200,
            )
            .await?;
        ensure!(work["request_id"] == id, "work routed to incorrect request");
        let incoming: EnvelopeV1 = serde_json::from_value(work["envelope"].clone())?;
        let clear = incoming.open(
            key(11 + link as u8).verifying_key(),
            &agreement(21 + mac as u8),
            LINKS[link].0,
            MACS[mac],
            MACS[mac],
            now(),
        )?;
        ensure!(
            clear == br#"{"method":"GET","path":"/v1/tasks"}"#,
            "request plaintext mismatch"
        );
        self.call(
            code,
            MACS[mac],
            None,
            "POST",
            &format!("/v1/work/{id}/receipt"),
            vec![],
            200,
        )
        .await?;
        let response = EnvelopeV1::seal_random(
            Context {
                route: MACS[mac],
                sender: MACS[mac],
                recipient: LINKS[link].0,
                request_id: id,
                kind: "response",
                expires_at_ms: now() + 120_000,
            },
            br#"{"status":200,"body":{"synthetic":true}}"#,
            &key(code),
            &agreement(31 + link as u8).public_key(),
        )?;
        self.call(
            code,
            MACS[mac],
            None,
            "POST",
            &format!("/v1/work/{id}/response"),
            serde_json::to_vec(&response)?,
            200,
        )
        .await?;
        let result = self
            .call(
                11 + link as u8,
                LINKS[link].0,
                Some(owner(mac)),
                "GET",
                &format!("/v1/requests/{id}"),
                vec![],
                200,
            )
            .await?;
        ensure!(
            result["status"] == "completed",
            "missing completed response"
        );
        let response: EnvelopeV1 = serde_json::from_value(result["response"].clone())?;
        let clear = response.open(
            key(code).verifying_key(),
            &agreement(31 + link as u8),
            MACS[mac],
            LINKS[link].0,
            MACS[mac],
            now(),
        )?;
        ensure!(
            serde_json::from_slice::<Value>(&clear)?["body"]["synthetic"] == true,
            "response plaintext mismatch"
        );
        Ok(())
    }
    async fn initial(&self) -> Result<()> {
        for mac in 1..3 {
            self.call(1+mac as u8,MACS[mac],Some(owner(mac)),"POST","/v1/macs",serde_json::to_vec(&json!({"mac_id":MACS[mac],"signing_key":public(1+mac as u8),"agreement_key":B64.encode(agreement(21+mac as u8).public_key().to_sec1_point(false).as_bytes())}))?,201).await?;
        }
        for who in 0..2 {
            let value = self
                .http
                .get("http://relay:18081/v1/macs?owner_sub=foreign")
                .bearer_auth(&self.tokens[who])
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await?;
            let found = value["macs"].as_array().context("mac directory")?;
            ensure!(
                found.len() == if who == 0 { 2 } else { 1 },
                "owner directory leaked or omitted Macs"
            );
            ensure!(
                found.iter().all(|v| MACS
                    .iter()
                    .enumerate()
                    .any(|(m, id)| owner(m) == who && v["mac_id"] == *id)),
                "foreign Mac in directory"
            );
        }
        for (link, (id, mac)) in LINKS.iter().enumerate() {
            self.call(1+*mac as u8,MACS[*mac],None,"POST","/v1/devices",serde_json::to_vec(&json!({"device_id":id,"owner_sub":self.fixture["owners"][owner(*mac)]["id"],"signing_key":public(11+link as u8)}))?,201).await?;
            let rid = format!("system-roundtrip-{link}");
            let body = self.envelope(link, *mac, &rid)?;
            self.submit(link, body.clone(), 202).await?;
            self.submit(link, body.clone(), 202).await?;
            self.complete(link, &rid).await?;
            self.submit(link, body, 200).await?;
            self.submit(link, self.envelope(link, *mac, &rid)?, 409)
                .await?;
        }
        for target in [1, 2] {
            self.submit(
                0,
                self.envelope(0, target, &format!("system-reroute-{target}"))?,
                400,
            )
            .await?;
        }
        self.call(
            11,
            LINKS[0].0,
            Some(1),
            "POST",
            "/v1/requests",
            self.envelope(0, 0, "system-wrong-owner")?,
            401,
        )
        .await?;
        self.call(
            15,
            LINKS[4].0,
            Some(1),
            "GET",
            "/v1/requests/system-roundtrip-0",
            vec![],
            404,
        )
        .await?;
        ensure!(
            self.call_epoch(
                &self.epoch,
                2,
                MACS[1],
                None,
                "DELETE",
                &format!("/v1/devices/{}", LINKS[0].0),
                vec![]
            )
            .await?
            .0 >= 400,
            "other Mac revoked a foreign link"
        );
        self.call(
            1,
            MACS[0],
            None,
            "DELETE",
            &format!("/v1/devices/{}", LINKS[0].0),
            vec![],
            204,
        )
        .await?;
        self.submit(0, self.envelope(0, 0, "system-revoked")?, 401)
            .await?;
        let pending = self.envelope(1, 1, "system-pending-restart")?;
        self.submit(1, pending.clone(), 202).await?;
        save(
            "client-state.json",
            json!({"epoch":B64.encode(self.epoch),"pending":B64.encode(pending)}),
        )?;
        save(
            "initial.json",
            json!({"phase":"initial","status":"passed","macs":3,"physical_clients_owner_a":2,"link_identities":5,"checks":["real_oidc","encrypted_roundtrip","duplicate_same_bytes","changed_bytes_conflict","owner_directory","same_owner_reroute_denied","foreign_owner_denied","cross_mac_revoke_denied","single_link_revoke"]}),
        )
    }
    async fn restart(&self) -> Result<()> {
        let state: Value = serde_json::from_slice(&std::fs::read("/system/client-state.json")?)?;
        let old: [u8; 16] = B64
            .decode(state["epoch"].as_str().context("saved epoch")?)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("epoch"))?;
        ensure!(old != self.epoch, "relay restart did not change epoch");
        ensure!(
            self.call_epoch(&old, 2, MACS[1], None, "GET", "/v1/work?wait_s=0", vec![])
                .await?
                .0
                == 401,
            "old epoch accepted"
        );
        self.submit(0, self.envelope(0, 0, "system-revoked-after-restart")?, 401)
            .await?;
        self.call(
            12,
            LINKS[1].0,
            Some(0),
            "GET",
            "/v1/requests/system-pending-restart",
            vec![],
            404,
        )
        .await?;
        let body = B64.decode(state["pending"].as_str().context("saved request")?)?;
        self.submit(1, body.clone(), 202).await?;
        self.complete(1, "system-pending-restart").await?;
        self.submit(1, body, 200).await?;
        save(
            "restart.json",
            json!({"phase":"restart","status":"passed","checks":["epoch_rotated","old_epoch_rejected","revocation_persisted","volatile_queue_missing_explicit","same_envelope_resubmitted_and_completed"],"limit":"synthetic Mac; does not claim real daemon exactly-once execution"}),
        )
    }
    async fn perf(self: Arc<Self>) -> Result<()> {
        let started = Instant::now();
        let mut jobs = vec![];
        // Bounded local profile: 8 concurrent authenticated, signed short polls, 20 each.
        for _ in 0..8 {
            let driver = self.clone();
            jobs.push(tokio::spawn(async move {
                let mut times = vec![];
                for _ in 0..20 {
                    let start = Instant::now();
                    driver
                        .call(
                            12,
                            LINKS[1].0,
                            Some(0),
                            "GET",
                            "/v1/changes?since=0&wait_s=0",
                            vec![],
                            200,
                        )
                        .await?;
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                }
                Ok::<_, anyhow::Error>(times)
            }));
        }
        let mut times = vec![];
        for job in jobs {
            times.extend(job.await??);
        }
        times.sort_by(f64::total_cmp);
        let elapsed = started.elapsed().as_secs_f64();
        let cursor = self
            .call(
                12,
                LINKS[1].0,
                Some(0),
                "GET",
                "/v1/changes?since=0&wait_s=0",
                vec![],
                200,
            )
            .await?["revision"]
            .as_u64()
            .context("revision")?;
        let mut waits = vec![];
        for _ in 0..8 {
            let driver = self.clone();
            waits.push(tokio::spawn(async move {
                let start = Instant::now();
                let value = driver
                    .call(
                        12,
                        LINKS[1].0,
                        Some(0),
                        "GET",
                        &format!("/v1/changes?since={cursor}&wait_s=1"),
                        vec![],
                        200,
                    )
                    .await?;
                ensure!(
                    value["changes"].as_array().context("changes")?.is_empty(),
                    "idle long poll leaked changes"
                );
                let millis = start.elapsed().as_secs_f64() * 1000.0;
                ensure!(millis >= 800.0, "idle poll returned without waiting");
                Ok::<_, anyhow::Error>(millis)
            }));
        }
        let mut wait_times = vec![];
        for job in waits {
            wait_times.push(job.await??);
        }
        wait_times.sort_by(f64::total_cmp);
        save(
            "perf.json",
            json!({"phase":"perf","status":"passed","profile":"8x20 signed short changes polls, local only","requests":times.len(),"errors":0,"concurrency":8,"elapsed_seconds":elapsed,"requests_per_second":times.len() as f64/elapsed,"p50_ms":times[times.len()/2],"p95_ms":times[(times.len()*95/100).min(times.len()-1)],"idle_long_poll":{"requests":8,"wait_s":1,"p50_ms":wait_times[4],"p95_ms":wait_times[7]},"limit":"not a capacity claim or 100k-user forecast"}),
        )
    }
}
mod load;
mod capacity;
mod account_reset;

#[tokio::main]
async fn main() -> Result<()> {
    let phase = std::env::args()
        .nth(1)
        .context("phase initial|restart|perf required")?;
    ensure!(
        ["initial", "restart", "perf", "load", "capacity", "account-reset-before", "account-reset-after"].contains(&phase.as_str()),
        "invalid phase"
    );
    if phase == "capacity" {
        return capacity::entry().await;
    }
    if phase == "load" {
        return load::entry().await;
    }
    let driver = Arc::new(Driver::open().await?);
    match phase.as_str() {
        "initial" => driver.initial().await,
        "restart" => driver.restart().await,
        "perf" => driver.perf().await,
        "account-reset-before" => account_reset::execute(&driver, false).await,
        "account-reset-after" => account_reset::execute(&driver, true).await,

        _ => unreachable!(),
    }
}
