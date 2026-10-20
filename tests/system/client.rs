//! Disposable system-test client. Uses real OIDC, signed HTTP and encrypted envelopes.
use alondra_e2e::sign_http_challenge;
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
            epoch,
        })
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
    async fn link_access(&self, link: usize, who: usize, expected: u16) -> Result<Value> {
        self.call(
            11 + link as u8,
            LINKS[link].0,
            Some(who),
            "DELETE",
            "/v1/iroh-node",
            vec![],
            expected,
        )
        .await
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
            self.link_access(link, owner(*mac), 204).await?;
        }
        self.link_access(0, 1, 401).await?;
        for (method, path) in [
            ("POST", "/v1/requests"),
            ("GET", "/v1/changes?since=0&wait_s=0"),
            ("GET", "/v1/work?wait_s=0"),
        ] {
            let (status, value) = self
                .call_epoch(&self.epoch, 12, LINKS[1].0, Some(0), method, path, vec![])
                .await?;
            ensure!(
                status == 404 && value["error"] == "mailbox_removed",
                "{method} {path}: expected mailbox_removed, received {status}"
            );
        }
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
        self.link_access(0, 0, 401).await?;
        self.link_access(1, 0, 204).await?;
        save(
            "client-state.json",
            json!({"epoch":B64.encode(self.epoch)}),
        )?;
        save(
            "initial.json",
            json!({"phase":"initial","status":"passed","macs":3,"physical_clients_owner_a":2,"link_identities":5,"checks":["real_oidc","owner_directory","signed_link_access","foreign_owner_denied","mailbox_removed","cross_mac_revoke_denied","single_link_revoke"]}),
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
            self.call_epoch(&old, 2, MACS[1], None, "DELETE", "/v1/iroh-node", vec![])
                .await?
                .0
                == 401,
            "old epoch accepted"
        );
        self.call(2, MACS[1], None, "DELETE", "/v1/iroh-node", vec![], 204)
            .await?;
        self.link_access(0, 0, 401).await?;
        self.link_access(1, 0, 204).await?;
        save(
            "restart.json",
            json!({"phase":"restart","status":"passed","checks":["epoch_rotated","old_epoch_rejected","current_epoch_accepted","revocation_persisted","enrollment_persisted"],"limit":"synthetic Mac and client keys; no iroh transport"}),
        )
    }
    async fn perf(self: Arc<Self>) -> Result<()> {
        let started = Instant::now();
        let mut jobs = vec![];
        // Bounded local profile: 8 concurrent authenticated, signed client requests, 20 each.
        for _ in 0..8 {
            let driver = self.clone();
            jobs.push(tokio::spawn(async move {
                let mut times = vec![];
                for _ in 0..20 {
                    let start = Instant::now();
                    driver.link_access(1, 0, 204).await?;
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
        save(
            "perf.json",
            json!({"phase":"perf","status":"passed","profile":"8x20 signed client DELETE /v1/iroh-node, local only","requests":times.len(),"errors":0,"concurrency":8,"elapsed_seconds":elapsed,"requests_per_second":times.len() as f64/elapsed,"p50_ms":times[times.len()/2],"p95_ms":times[(times.len()*95/100).min(times.len()-1)],"limit":"not a capacity claim or 100k-user forecast"}),
        )
    }
}
mod account_reset;

#[tokio::main]
async fn main() -> Result<()> {
    let phase = std::env::args()
        .nth(1)
        .context("phase initial|restart|perf required")?;
    ensure!(
        ["initial", "restart", "perf", "account-reset-before", "account-reset-after"].contains(&phase.as_str()),
        "invalid phase"
    );
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
