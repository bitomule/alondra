//! Bounded production-protocol load, with explicit overload and recovery accounting.
use super::*;
use std::collections::BTreeMap;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::RwLock;

#[derive(Clone)]
struct Link {
    owner: usize,
    mac: String,
    phone: String,
    mac_key: u8,
    phone_key: u8,
    mac_agreement: u8,
    phone_agreement: u8,
}
fn links(accounts: usize) -> Vec<Link> {
    let mut result = vec![];
    for owner in 0..accounts {
        for mac in 0..2 {
            for client in 0..2 {
                let index = owner * 4 + mac * 2 + client;
                result.push(Link {
                    owner,
                    mac: format!("system-load-mac-{owner}-{mac}"),
                    phone: format!("system-load-phone-{owner}-{mac}-{client}"),
                    mac_key: (20 + owner * 2 + mac) as u8,
                    phone_key: (80 + index) as u8,
                    mac_agreement: (120 + owner * 2 + mac) as u8,
                    phone_agreement: (160 + index) as u8,
                });
            }
        }
    }
    result
}
#[derive(Default)]
struct Counters {
    cohorts: [Vec<u64>; 4],
    account_last_success: BTreeMap<usize, u64>,
    account_largest_gap: BTreeMap<usize, u64>,
    elapsed_ms: u64,
    count: u64,
    total_ms: f64,
    histogram: Vec<u64>,
    statuses: BTreeMap<u16, u64>,
    accounts: BTreeMap<usize, [u64; 4]>,
    expected_denials: u64,
    errors: u64,
    holds: Vec<(u64, u64)>,
}
impl Counters {
    fn record(
        &mut self,
        account: usize,
        status: Option<u16>,
        start: Instant,
        origin: Instant,
        held: bool,
        overload: bool,
        revoked: bool,
    ) {
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        if self.histogram.is_empty() {
            self.histogram.resize(60_001, 0);
        }
        self.histogram[(elapsed.ceil() as usize).min(60_000)] += 1;
        self.count += 1;
        self.total_ms += elapsed;
        self.elapsed_ms = origin.elapsed().as_millis() as u64;
        let accepted = matches!(status, Some(200 | 204));
        let cohort = if accepted {
            0
        } else if overload {
            1
        } else if revoked && status == Some(401) {
            2
        } else {
            3
        };
        if self.cohorts[cohort].is_empty() {
            self.cohorts[cohort].resize(60_001, 0);
        }
        self.cohorts[cohort][(elapsed.ceil() as usize).min(60_000)] += 1;
        if accepted {
            let previous = self
                .account_last_success
                .insert(account, self.elapsed_ms)
                .unwrap_or(0);
            let gap = self.account_largest_gap.entry(account).or_default();
            *gap = (*gap).max(self.elapsed_ms - previous);
        }
        let owner = self.accounts.entry(account).or_default();
        match status {
            Some(code) => {
                *self.statuses.entry(code).or_default() += 1;
                if code == 200 || code == 204 {
                    owner[0] += 1;
                } else if overload {
                    owner[1] += 1;
                } else if code == 401 && revoked {
                    owner[3] += 1;
                    self.expected_denials += 1;
                } else {
                    owner[2] += 1;
                    self.errors += 1;
                }
                if held && matches!(code, 200 | 204) && elapsed >= 19_000.0 {
                    self.holds.push((
                        start.duration_since(origin).as_millis() as u64,
                        origin.elapsed().as_millis() as u64,
                    ));
                }
            }
            None => {
                owner[2] += 1;
                self.errors += 1;
            }
        }
    }
    fn json(&self) -> Value {
        let percentile = |numerator: u64| {
            let target = (self.count * numerator).div_ceil(100);
            let mut total = 0;
            for (ms, count) in self.histogram.iter().enumerate() {
                total += count;
                if total >= target {
                    return ms;
                }
            }
            0
        };
        let mut points = Vec::with_capacity(self.holds.len() * 2);
        for (a, b) in &self.holds {
            points.push((*a, 1i64));
            points.push((*b, -1));
        }
        points.sort_unstable();
        let (mut active, mut peak) = (0, 0);
        for (_, delta) in points {
            active += delta;
            peak = peak.max(active);
        }
        let summaries:Vec<Value>=self.cohorts.iter().enumerate().map(|(index,hist)| {
            let count:u64=hist.iter().sum();
            let percentile=|p:u64|{let target=(count*p).div_ceil(100);let mut sum=0;for (ms,n) in hist.iter().enumerate(){sum+=n;if sum>=target{return ms;}}0};
            json!({"cohort":(["accepted","expected_overload","expected_revocation","unexpected"][index]),
                "count":count,"p50_ms":percentile(50),"p95_ms":percentile(95),"p99_ms":percentile(99)})
        }).collect();
        let fairness:Vec<Value>=self.accounts.iter().map(|(account,counts)|json!({"account":account,
            "accepted":counts[0],"accepted_per_minute":counts[0] as f64/(self.elapsed_ms.max(1) as f64/60_000.0),
            "largest_no_success_gap_ms":self.account_largest_gap.get(account).copied().unwrap_or(0)
                .max(self.elapsed_ms-self.account_last_success.get(account).copied().unwrap_or(0))})).collect();
        json!({"offered_requests":self.count,"completed_http_responses":self.statuses.values().sum::<u64>(),
            "accepted_responses":self.statuses.get(&200).unwrap_or(&0)+self.statuses.get(&204).unwrap_or(&0),"latency_cohorts":summaries,"account_fairness":fairness,"requests":self.count,"statuses":self.statuses,"unexpected_errors":self.errors,
            "p50_ms":percentile(50),"p95_ms":percentile(95),"p99_ms":percentile(99),
            "mean_ms":self.total_ms/self.count.max(1) as f64,"accounts":self.accounts,"account_columns":["accepted","expected_overload","unexpected_error","expected_revoked"],"expected_revocation_denials":self.expected_denials,
            "completed_idle_holds_19s":self.holds.len(),"overlapping_successful_idle_holds":peak})
    }
}
fn overload(result: &Result<(u16, Value)>) -> bool {
    matches!(result,Ok((429,value)) if matches!(value["error"].as_str(),Some("too_many_polls"|"queue_full")))
        || matches!(result,Ok((503,value)) if value["error"]=="server_busy")
}
static CRYPTO_RETRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[allow(clippy::too_many_arguments)]
async fn reliable(
    driver: &Driver,
    code: u8,
    id: &str,
    who: Option<usize>,
    method: &str,
    path: &str,
    body: Vec<u8>,
    expected: u16,
) -> Result<Value> {
    let mut last_overload = None;
    for attempt in 0..120 {
        let result = driver
            .call_epoch(&driver.epoch, code, id, who, method, path, body.clone())
            .await;
        if overload(&result) {
            last_overload = result.as_ref().ok().map(|(status, value)| {
                (
                    *status,
                    value["error"].as_str().unwrap_or("unknown").to_owned(),
                )
            });
            CRYPTO_RETRIES.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(250 + (attempt % 7) * 19)).await;
            continue;
        }
        let (status, value) = result?;
        ensure!(
            status == expected,
            "mixed operation {method} {path}: status {status}, expected {expected}"
        );
        return Ok(value);
    }
    anyhow::bail!(
        "mixed operation {method} {path}: 120 overload retries exhausted; last={last_overload:?}"
    )
}
type Current = Arc<RwLock<Arc<Driver>>>;
async fn get(current: &Current) -> Arc<Driver> {
    current.read().await.clone()
}
async fn setup(driver: &Driver, links: &[Link]) -> Result<()> {
    for (index, link) in links.iter().enumerate() {
        if index % 2 == 0 {
            driver.call(link.mac_key,&link.mac,Some(link.owner),"POST","/v1/macs",
                serde_json::to_vec(&json!({"mac_id":link.mac,"signing_key":public(link.mac_key),
                    "agreement_key":B64.encode(agreement(link.mac_agreement).public_key().to_sec1_point(false).as_bytes())}))?,201).await?;
        }
        driver.call(link.mac_key,&link.mac,None,"POST","/v1/devices",serde_json::to_vec(&json!({
            "device_id":link.phone,"owner_sub":driver.fixture["owners"][link.owner]["id"],"signing_key":public(link.phone_key)}))?,201).await?;
    }
    Ok(())
}
async fn encrypted_roundtrip(driver: &Driver, link: &Link, id: &str) -> Result<()> {
    encrypted_roundtrip_with_envelope(driver, link, id, None).await
}
async fn encrypted_roundtrip_with_envelope(
    driver: &Driver,
    link: &Link,
    id: &str,
    existing: Option<Vec<u8>>,
) -> Result<()> {
    let plain = serde_json::to_vec(&json!({"synthetic":true,"owner":link.owner,"request":id}))?;
    let envelope = EnvelopeV1::seal_random(
        Context {
            route: &link.mac,
            sender: &link.phone,
            recipient: &link.mac,
            request_id: id,
            kind: "request",
            expires_at_ms: now() + 120_000,
        },
        &plain,
        &key(link.phone_key),
        &agreement(link.mac_agreement).public_key(),
    )?;
    let bytes = existing.unwrap_or(serde_json::to_vec(&envelope)?);
    reliable(
        driver,
        link.phone_key,
        &link.phone,
        Some(link.owner),
        "POST",
        "/v1/requests",
        bytes.clone(),
        202,
    )
    .await?;
    reliable(
        driver,
        link.phone_key,
        &link.phone,
        Some(link.owner),
        "POST",
        "/v1/requests",
        bytes.clone(),
        202,
    )
    .await?;
    let work = reliable(
        driver,
        link.mac_key,
        &link.mac,
        None,
        "GET",
        "/v1/work?wait_s=0",
        vec![],
        200,
    )
    .await?;
    ensure!(
        work["request_id"] == id,
        "load work crossed request identity"
    );
    let incoming: EnvelopeV1 = serde_json::from_value(work["envelope"].clone())?;
    ensure!(
        incoming.open(
            key(link.phone_key).verifying_key(),
            &agreement(link.mac_agreement),
            &link.phone,
            &link.mac,
            &link.mac,
            now()
        )? == plain,
        "load request plaintext mismatch"
    );
    reliable(
        driver,
        link.mac_key,
        &link.mac,
        None,
        "POST",
        &format!("/v1/work/{id}/receipt"),
        vec![],
        200,
    )
    .await?;
    let reply = EnvelopeV1::seal_random(
        Context {
            route: &link.mac,
            sender: &link.mac,
            recipient: &link.phone,
            request_id: id,
            kind: "response",
            expires_at_ms: now() + 120_000,
        },
        &plain,
        &key(link.mac_key),
        &agreement(link.phone_agreement).public_key(),
    )?;
    reliable(
        driver,
        link.mac_key,
        &link.mac,
        None,
        "POST",
        &format!("/v1/work/{id}/response"),
        serde_json::to_vec(&reply)?,
        200,
    )
    .await?;
    let result = reliable(
        driver,
        link.phone_key,
        &link.phone,
        Some(link.owner),
        "GET",
        &format!("/v1/requests/{id}"),
        vec![],
        200,
    )
    .await?;
    let returned: EnvelopeV1 = serde_json::from_value(result["response"].clone())?;
    ensure!(
        returned.open(
            key(link.mac_key).verifying_key(),
            &agreement(link.phone_agreement),
            &link.mac,
            &link.phone,
            &link.mac,
            now()
        )? == plain,
        "load response plaintext mismatch"
    );
    reliable(
        driver,
        link.phone_key,
        &link.phone,
        Some(link.owner),
        "POST",
        "/v1/requests",
        bytes,
        200,
    )
    .await?;
    Ok(())
}
async fn burst(
    current: Current,
    links: Arc<Vec<Link>>,
    count: usize,
    counters: Arc<Mutex<Counters>>,
    origin: Instant,
) -> Result<()> {
    let mut jobs = vec![];
    for lane in 0..64 {
        let (current, links, counters) = (current.clone(), links.clone(), counters.clone());
        jobs.push(tokio::spawn(async move {
            for index in (lane..count).step_by(64) {
                let slot = index % links.len();
                let link = &links[if slot == 3 { 2 } else { slot }];
                let d = get(&current).await;
                let start = Instant::now();
                let response = d
                    .call_epoch(
                        &d.epoch,
                        link.phone_key,
                        &link.phone,
                        Some(link.owner),
                        "GET",
                        "/v1/changes?since=0&wait_s=0",
                        vec![],
                    )
                    .await;
                counters.lock().unwrap().record(
                    link.owner,
                    response.as_ref().ok().map(|r| r.0),
                    start,
                    origin,
                    false,
                    overload(&response),
                    false,
                );
            }
        }));
    }
    for job in jobs {
        job.await?;
    }
    Ok(())
}
async fn restart_probe(driver: Arc<Driver>, links: &[Link]) -> Result<Value> {
    let pending = &links[4];
    let id = "system-load-restart-pending";
    let plain = serde_json::to_vec(&json!({"synthetic":true,"owner":pending.owner,"request":id}))?;
    let body = serde_json::to_vec(&EnvelopeV1::seal_random(
        Context {
            route: &pending.mac,
            sender: &pending.phone,
            recipient: &pending.mac,
            request_id: id,
            kind: "request",
            expires_at_ms: now() + 120_000,
        },
        &plain,
        &key(pending.phone_key),
        &agreement(pending.mac_agreement).public_key(),
    )?)?;
    driver
        .call(
            pending.phone_key,
            &pending.phone,
            Some(pending.owner),
            "POST",
            "/v1/requests",
            body.clone(),
            202,
        )
        .await?;
    let mut active = vec![];
    for index in 0..8 {
        let link = links[4 + index % (links.len() - 4)].clone();
        let d = driver.clone();
        let status = d
            .call(
                link.phone_key,
                &link.phone,
                Some(link.owner),
                "GET",
                "/v1/changes?since=0&wait_s=0",
                vec![],
                200,
            )
            .await?;
        let revision = status["revision"].as_u64().context("restart cursor")?;
        active.push(tokio::spawn(async move {
            d.call_epoch(
                &d.epoch,
                link.phone_key,
                &link.phone,
                Some(link.owner),
                "GET",
                &format!("/v1/changes?since={revision}&wait_s=25"),
                vec![],
            )
            .await
        }));
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    ensure!(
        active.iter().all(|job| !job.is_finished()),
        "restart probe did not establish open requests"
    );
    save(
        "load-restart-request.json",
        json!({"operation":"restart_disposable_relay"}),
    )?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !Path::new("/system/load-restart-complete.json").exists() {
        ensure!(
            Instant::now() < deadline,
            "test supervisor did not acknowledge restart"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mut interrupted = 0;
    for job in active {
        match job.await? {
            Err(_) => interrupted += 1,
            Ok((status, _)) => ensure!(
                matches!(status, 200 | 503),
                "unexpected status during controlled restart"
            ),
        }
    }
    ensure!(
        interrupted > 0,
        "controlled restart did not interrupt any open request"
    );
    let recovered = Driver::open().await?;
    ensure!(
        recovered.epoch != driver.epoch,
        "relay epoch did not rotate"
    );
    recovered
        .call_epoch(
            &driver.epoch,
            pending.phone_key,
            &pending.phone,
            Some(pending.owner),
            "GET",
            "/v1/changes?since=0&wait_s=0",
            vec![],
        )
        .await
        .and_then(|(status, _)| {
            ensure!(status == 401, "old relay epoch accepted after restart");
            Ok(())
        })?;
    recovered
        .call(
            pending.phone_key,
            &pending.phone,
            Some(pending.owner),
            "GET",
            &format!("/v1/requests/{id}"),
            vec![],
            404,
        )
        .await?;
    encrypted_roundtrip_with_envelope(&recovered, pending, id, Some(body)).await?;
    for index in [0, 3] {
        let revoked = &links[index];
        recovered
            .call(
                revoked.phone_key,
                &revoked.phone,
                Some(revoked.owner),
                "GET",
                "/v1/changes?since=0&wait_s=0",
                vec![],
                401,
            )
            .await?;
    }
    Ok(
        json!({"interrupted_open_requests":interrupted,"epoch_rotated":true,"old_epoch_rejected":true,
        "revocations_persisted":true,"volatile_pending_missing_explicit":true,"identical_ciphertext_resubmitted_and_decrypted":true}),
    )
}
pub async fn run(driver: Arc<Driver>) -> Result<()> {
    let profile: Value = serde_json::from_slice(&std::fs::read("/system/load-profile.json")?)?;
    let seconds = profile["duration_seconds"]
        .as_u64()
        .context("load duration")?;
    ensure!(
        (10..=3600).contains(&seconds),
        "load duration outside explicit bounded range"
    );
    let concurrency = profile["change_concurrency"]
        .as_u64()
        .context("load concurrency")? as usize;
    ensure!(
        (1..=512).contains(&concurrency),
        "load concurrency outside bounded range"
    );
    let count = profile["burst_requests"].as_u64().context("burst count")? as usize;
    ensure!(
        (1..=50_000).contains(&count),
        "burst count outside bounded range"
    );
    let accounts = driver.tokens.len();
    ensure!((2..=16).contains(&accounts), "account count");
    let links = Arc::new(links(accounts));
    setup(&driver, &links).await?;
    let current = Arc::new(RwLock::new(driver));
    let stop = Arc::new(AtomicBool::new(false));
    let refresh = {
        let current = current.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let mut renewals = 0;
            while !stop.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let next = get(&current).await.refresh().await?;
                *current.write().await = Arc::new(next);
                renewals += 1;
            }
            Ok::<_, anyhow::Error>(renewals)
        })
    };
    let origin = Instant::now();
    let deadline = origin + Duration::from_secs(seconds);
    let polls = Arc::new(Mutex::new(Counters::default()));
    let bursts = Arc::new(Mutex::new(Counters::default()));
    let work_polls = Arc::new(Mutex::new(Counters::default()));
    let revoked_during_load = Arc::new(AtomicBool::new(false));
    let mut workers = vec![];
    for lane in 0..concurrency {
        let (current, links, polls) = (current.clone(), links.clone(), polls.clone());
        let revoked = revoked_during_load.clone();
        workers.push(tokio::spawn(async move {
            let link = &links[lane % links.len()];
            let mut cursor = 0;
            // Rotate re-admission with bounded stagger instead of a tight retry storm.
            tokio::time::sleep(Duration::from_millis((lane % 31) as u64 * 3)).await;
            while Instant::now() < deadline {
                if lane % links.len() == 3 && revoked.load(Ordering::Relaxed) {
                    break;
                }
                let d = get(&current).await;
                let start = Instant::now();
                let response = d
                    .call_epoch(
                        &d.epoch,
                        link.phone_key,
                        &link.phone,
                        Some(link.owner),
                        "GET",
                        &format!("/v1/changes?since={cursor}&wait_s=20"),
                        vec![],
                    )
                    .await;
                let code = response.as_ref().ok().map(|v| v.0);
                if let Ok((200, value)) = &response {
                    cursor = value["revision"].as_u64().unwrap_or(cursor);
                }
                let idle_reply = matches!(&response, Ok((200, value))
                    if value["changes"].as_array().is_some_and(Vec::is_empty));
                polls.lock().unwrap().record(
                    link.owner,
                    code,
                    start,
                    origin,
                    idle_reply,
                    overload(&response),
                    lane % links.len() == 3 && revoked.load(Ordering::Relaxed),
                );
                tokio::time::sleep(Duration::from_millis(if matches!(code, Some(429 | 503)) {
                    250 + (lane % 17) as u64 * 13
                } else {
                    50 + (lane % 11) as u64 * 5
                }))
                .await;
            }
        }));
    }
    for lane in 0..accounts * 2 {
        let (current, links, work_polls) = (current.clone(), links.clone(), work_polls.clone());
        workers.push(tokio::spawn(async move {
            let link = &links[(lane / 2) * 4 + 2];
            while Instant::now() < deadline {
                let d = get(&current).await;
                let start = Instant::now();
                let response = d
                    .call_epoch(
                        &d.epoch,
                        link.mac_key,
                        &link.mac,
                        None,
                        "GET",
                        "/v1/work?wait_s=20",
                        vec![],
                    )
                    .await;
                let code = response.as_ref().ok().map(|v| v.0);
                work_polls.lock().unwrap().record(
                    link.owner,
                    code,
                    start,
                    origin,
                    true,
                    overload(&response),
                    false,
                );
                tokio::time::sleep(Duration::from_millis(if matches!(code, Some(429 | 503)) {
                    400 + (lane % 13) as u64 * 17
                } else {
                    200 + (lane % 7) as u64 * 13
                }))
                .await;
            }
        }));
    }
    let mut crypto_jobs = vec![];
    let mixed_counts = Arc::new(Mutex::new(vec![[0u64; 2]; accounts]));
    for account in 0..accounts {
        let (current, links) = (current.clone(), links.clone());
        let mixed_counts = mixed_counts.clone();
        crypto_jobs.push(tokio::spawn(async move {
            let mut completed = 0;
            for iteration in 0..20 {
                if Instant::now() >= deadline {
                    break;
                }
                let d = get(&current).await;
                mixed_counts.lock().unwrap()[account][0] += 1;
                encrypted_roundtrip(
                    &d,
                    &links[account * 4],
                    &format!("system-load-mixed-{account}-{iteration}"),
                )
                .await?;
                completed += 1;
                mixed_counts.lock().unwrap()[account][1] += 1;
                tokio::time::sleep(Duration::from_secs((seconds / 20).max(1))).await;
            }
            Ok::<_, anyhow::Error>(completed)
        }));
    }
    let revoke_job = {
        let (current, links, revoked) =
            (current.clone(), links.clone(), revoked_during_load.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(seconds / 2)).await;
            revoked.store(true, Ordering::Relaxed);
            let d = get(&current).await;
            let link = &links[3];
            let began = Instant::now();
            reliable(
                &d,
                link.mac_key,
                &link.mac,
                None,
                "DELETE",
                &format!("/v1/devices/{}", link.phone),
                vec![],
                204,
            )
            .await?;
            reliable(
                &d,
                link.phone_key,
                &link.phone,
                Some(link.owner),
                "GET",
                "/v1/changes?since=0&wait_s=0",
                vec![],
                401,
            )
            .await?;
            let survivor = &links[2];
            reliable(
                &d,
                survivor.phone_key,
                &survivor.phone,
                Some(survivor.owner),
                "GET",
                "/v1/changes?since=0&wait_s=0",
                vec![],
                200,
            )
            .await?;
            Ok::<_, anyhow::Error>(began.elapsed().as_secs_f64())
        })
    };
    let burst_job = {
        let (current, links, bursts) = (current.clone(), links.clone(), bursts.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs((seconds / 4).min(30))).await;
            burst(
                current.clone(),
                links.clone(),
                count,
                bursts.clone(),
                origin,
            )
            .await?;
            tokio::time::sleep(Duration::from_secs(seconds / 2)).await;
            burst(current, links, count, bursts, origin).await
        })
    };
    let mut failures = Vec::<String>::new();
    for worker in workers {
        if worker.await.is_err() {
            failures.push("poll worker failed to join".into());
        }
    }
    for (account, job) in crypto_jobs.into_iter().enumerate() {
        match job.await {
            Ok(Ok(_)) => (),
            Ok(Err(error)) => failures.push(format!("synthetic account {account}: {error}")),
            Err(_) => failures.push(format!(
                "synthetic account {account}: crypto worker failed to join"
            )),
        }
    }
    let revoke_seconds = match revoke_job.await {
        Ok(Ok(seconds)) => Some(seconds),
        Ok(Err(error)) => {
            failures.push(format!("revocation: {error}"));
            None
        }
        Err(_) => {
            failures.push("revocation worker failed to join".into());
            None
        }
    };
    match burst_job.await {
        Ok(Ok(())) => (),
        Ok(Err(error)) => failures.push(format!("burst: {error}")),
        Err(_) => failures.push("burst worker failed to join".into()),
    }
    stop.store(true, Ordering::Relaxed);
    let refreshes = match refresh.await {
        Ok(Ok(count)) => Some(count),
        Ok(Err(error)) => {
            failures.push(format!("OIDC refresh: {error}"));
            None
        }
        Err(_) => {
            failures.push("OIDC refresh worker failed to join".into());
            None
        }
    };
    let mixed_by_account = mixed_counts.lock().unwrap().clone();
    let mixed_roundtrips: u64 = mixed_by_account.iter().map(|counts| counts[1]).sum();
    let mixed_attempts: u64 = mixed_by_account.iter().map(|counts| counts[0]).sum();
    let poll_json = polls.lock().unwrap().json();
    let burst_json = bursts.lock().unwrap().json();
    let work_json = work_polls.lock().unwrap().json();
    save(
        "load-progress.json",
        json!({"phase":"steady_finished","polls":poll_json,"work_polls":work_json,"bursts":burst_json,
            "oidc_refresh_batches":refreshes,"mixed_roundtrips":mixed_roundtrips,
            "mixed_attempts":mixed_attempts,"mixed_account_columns":["attempted","completed"],
            "mixed_by_account":mixed_by_account,"revocation_under_load_seconds":revoke_seconds,
            "mixed_overload_retries":CRYPTO_RETRIES.load(Ordering::Relaxed),"failures":failures}),
    )?;
    ensure!(
        failures.is_empty(),
        "load workers failed; inspect load-progress.json"
    );
    let revoke_seconds = revoke_seconds.context("missing revocation observation")?;
    let refreshes = refreshes.context("missing refresh observation")?;
    ensure!(
        polls.lock().unwrap().errors == 0
            && bursts.lock().unwrap().errors == 0
            && work_polls.lock().unwrap().errors == 0,
        "unexpected load HTTP/transport errors; inspect load-progress.json"
    );
    for account in 0..accounts {
        ensure!(
            polls
                .lock()
                .unwrap()
                .accounts
                .get(&account)
                .is_some_and(|counts| counts[0] > 0),
            "an account never obtained an admitted poll under shared limits"
        );
    }
    if seconds >= 600 && profile["limits"] == "tuned" {
        ensure!(
            poll_json["overlapping_successful_idle_holds"]
                .as_u64()
                .unwrap_or(0)
                >= 200,
            "tuned profile did not sustain hundreds of successfully completed overlapping long polls"
        );
    }
    // Once pressure ends, every account must regain access, with real encrypted work.
    for (index, link) in links.iter().enumerate() {
        let d = get(&current).await;
        if index == 3 {
            d.call(
                link.phone_key,
                &link.phone,
                Some(link.owner),
                "GET",
                "/v1/changes?since=0&wait_s=0",
                vec![],
                401,
            )
            .await?;
            continue;
        }
        d.call(
            link.phone_key,
            &link.phone,
            Some(link.owner),
            "GET",
            "/v1/changes?since=0&wait_s=0",
            vec![],
            200,
        )
        .await?;
        if index % 4 == 0 {
            encrypted_roundtrip(&d, link, &format!("system-load-recovery-{index}")).await?;
        }
    }
    let d = get(&current).await;
    let revoked = &links[0];
    d.call(
        revoked.mac_key,
        &revoked.mac,
        None,
        "DELETE",
        &format!("/v1/devices/{}", revoked.phone),
        vec![],
        204,
    )
    .await?;
    d.call(
        revoked.phone_key,
        &revoked.phone,
        Some(revoked.owner),
        "GET",
        "/v1/changes?since=0&wait_s=0",
        vec![],
        401,
    )
    .await?;
    let survivor = &links[1];
    encrypted_roundtrip(&d, survivor, "system-load-survivor").await?;
    save(
        "load-progress.json",
        json!({"phase":"idle_before_restart","polls":poll_json,"work_polls":work_json,"bursts":burst_json}),
    )?;
    tokio::time::sleep(Duration::from_secs(15)).await;
    let restarted = restart_probe(d.clone(), &links).await?;
    save(
        "load.json",
        json!({"phase":"load","status":"passed","profile":profile,"accounts":accounts,"macs":accounts*2,"links":links.len(),
        "elapsed_seconds":origin.elapsed().as_secs_f64(),"polls":poll_json,"work_polls":work_json,"bursts":burst_json,"oidc_refresh_batches":refreshes,"mixed_encrypted_roundtrips":mixed_roundtrips,"mixed_overload_retries":CRYPTO_RETRIES.load(Ordering::Relaxed),"revocation_under_load_seconds":revoke_seconds,
        "recovery":{"all_links_readable_before_selective_revoke":true,"encrypted_roundtrips":accounts+1,"revoked_link_rejected":true,"other_link_preserved":true},
        "restart":restarted,"scope":"real management/Keycloak/relay protocol, synthetic Macs; no daemon/Claude/application UI"}),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overload_histogram_cannot_hide_accepted_latency() {
        let mut counters = Counters::default();
        let origin = Instant::now() - Duration::from_secs(30);
        for _ in 0..100 {
            counters.record(0, Some(429), Instant::now(), origin, false, true, false);
        }
        counters.record(
            0,
            Some(200),
            Instant::now() - Duration::from_secs(20),
            origin,
            true,
            false,
            false,
        );
        let value = counters.json();
        assert!(value["p50_ms"].as_u64().unwrap() < 10);
        assert!(value["latency_cohorts"][0]["p50_ms"].as_u64().unwrap() >= 20_000);
        assert_eq!(value["latency_cohorts"][1]["count"], 100);
        assert_eq!(value["unexpected_errors"], 0);
    }
    #[test]
    fn provider_failure_is_not_expected_overload() {
        assert!(overload(&Ok((503, json!({"error":"server_busy"})))));
        assert!(!overload(&Ok((
            503,
            json!({"error":{"code":"provider_unavailable"}})
        ))));
        assert!(!overload(&Ok((401, json!({"error":"unauthorized"})))));
        let mut counters = Counters::default();
        let now = Instant::now();
        counters.record(0, Some(503), now, now, false, false, false);
        assert_eq!(counters.errors, 1);
    }
    #[test]
    fn synthetic_matrix_keeps_every_link_and_mac_identity_distinct() {
        let links = links(16);
        let phones: std::collections::HashSet<_> = links.iter().map(|v| &v.phone).collect();
        let macs: std::collections::HashSet<_> = links.iter().map(|v| &v.mac).collect();
        assert_eq!((links.len(), phones.len(), macs.len()), (64, 64, 32));
        for link in &links {
            let _ = (
                key(link.mac_key),
                key(link.phone_key),
                agreement(link.mac_agreement),
                agreement(link.phone_agreement),
            );
        }
    }
}
