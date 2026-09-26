//! Bounded production-protocol load, with explicit overload and recovery accounting.
use super::*;
use std::collections::BTreeMap;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::RwLock;

#[derive(Default)]
struct Latency(BTreeMap<u64, u64>);
impl Latency {
    fn record(&mut self, duration: Duration) {
        *self.0.entry(duration.as_millis() as u64).or_default() += 1;
    }
    fn json(&self) -> Value {
        let count: u64 = self.0.values().sum();
        let percentile = |p: u64| {
            let target = (count * p).div_ceil(100);
            let mut n = 0;
            for (ms, c) in &self.0 {
                n += c;
                if n >= target {
                    return *ms;
                }
            }
            0
        };
        json!({"count":count,"p50_ms":percentile(50),"p95_ms":percentile(95),"p99_ms":percentile(99),"histogram_ms":self.0})
    }
}
#[derive(Default)]
struct MixedLatency {
    success: Latency,
    success_after_overload: Latency,
    failure: Latency,
}
impl MixedLatency {
    fn record(&mut self, success: bool, retries: u64, duration: Duration) {
        if success {
            self.success.record(duration);
            if retries > 0 {
                self.success_after_overload.record(duration);
            }
        } else {
            self.failure.record(duration);
        }
    }
    fn json(&self) -> Value {
        json!({"successful_roundtrips":self.success.json(),"successful_after_overload_subset":self.success_after_overload.json(),"failed_roundtrips":self.failure.json()})
    }
}
tokio::task_local! {static LOCAL_CRYPTO_RETRIES:std::cell::Cell<u64>;}

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
    failure_categories: BTreeMap<String, u64>,
    failure_buckets_30s: BTreeMap<u64, BTreeMap<String, u64>>,
}
impl Counters {
    fn record_response(
        &mut self,
        account: usize,
        response: &Result<(u16, Value)>,
        start: Instant,
        origin: Instant,
        held: bool,
        revoked: bool,
    ) {
        self.record(
            account,
            response.as_ref().ok().map(|r| r.0),
            start,
            origin,
            held,
            overload(response),
            revoked,
        );
        if let Some(category) = failure_category(response) {
            *self.failure_categories.entry(category.clone()).or_default() += 1;
            *self
                .failure_buckets_30s
                .entry(origin.elapsed().as_secs() / 30)
                .or_default()
                .entry(category)
                .or_default() += 1;
        }
    }
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
            "failure_categories":self.failure_categories,"failure_buckets_30s":self.failure_buckets_30s,
            "completed_idle_holds_19s":self.holds.len(),"overlapping_successful_idle_holds":peak})
    }
}
// Only public protocol codes and transport classes enter diagnostic artifacts.
// Never copy a provider body, URL, bearer token or error Display implementation.
fn failure_category(result: &Result<(u16, Value)>) -> Option<String> {
    match result {
        Ok((status, _)) if *status < 400 => None,
        Ok((status, value)) => {
            let code = match value["error"].as_str() {
                Some(
                    code @ ("too_many_polls"
                    | "queue_full"
                    | "server_busy"
                    | "account_access_unavailable"
                    | "account_access_denied"
                    | "unauthorized"
                    | "relay_epoch_changed"
                    | "stale_cursor"
                    | "not_found"),
                ) => code,
                _ => "unrecognized_code",
            };
            Some(format!("http_{status}_{code}"))
        }
        Err(error) => {
            let category = match error.downcast_ref::<reqwest::Error>() {
                Some(error) if error.is_timeout() => "transport_timeout",
                Some(error) if error.is_connect() => "transport_connect",
                Some(error) if error.is_body() => "transport_body",
                Some(error) if error.is_decode() => "transport_decode",
                _ => "transport_other",
            };
            Some(category.into())
        }
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
            let _ = LOCAL_CRYPTO_RETRIES.try_with(|count| count.set(count.get() + 1));
            tokio::time::sleep(Duration::from_millis(250 + (attempt % 7) * 19)).await;
            continue;
        }
        let category = failure_category(&result);
        let (status, value) =
            result.map_err(|_| anyhow::anyhow!("mixed operation {method} {path}: {category:?}"))?;
        ensure!(
            status == expected,
            "mixed operation {method} {path}: status {status}, expected {expected}, category={category:?}"
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
    encrypted_roundtrip_on_work(driver, link, id, existing, None).await
}
type ParkedWork = tokio::task::JoinHandle<Result<(u16, Value, Instant)>>;
async fn wakeup_bound<T, F>(deadline: Instant, operation: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::time::timeout_at(deadline.into(), operation)
        .await
        .context("parked delivery missed wakeup bound")?
}
async fn encrypted_roundtrip_on_work(
    driver: &Driver,
    link: &Link,
    id: &str,
    existing: Option<Vec<u8>>,
    parked: Option<(ParkedWork, Arc<Mutex<Latency>>, Instant)>,
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
    let published = Instant::now();
    let delivery_deadline = parked.as_ref().map(|(_, _, opened)| {
        std::cmp::min(
            published + Duration::from_secs(3),
            *opened + Duration::from_secs(20),
        )
    });
    if let Some((_, _, opened)) = &parked {
        ensure!(
            published.duration_since(*opened) < Duration::from_secs(10),
            "parked barrier consumed wakeup test budget"
        );
    }
    let submit_and_receive = async {
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
        let work = if let Some((parked, delivery_latency, _)) = parked {
            // Consume the SAME request proven Pending before the POST. Never hide a
            // missed wakeup by sending another GET or a retry after its timeout.
            let (status, work, received) =
                tokio::time::timeout(Duration::from_secs(30), parked).await???;
            ensure!(
                status == 200,
                "already parked Mac did not receive encrypted work"
            );
            ensure!(
                work["request_id"] == id,
                "parked delivery crossed request identity"
            );
            delivery_latency.lock().unwrap().record(
                received
                    .checked_duration_since(published)
                    .context("delivery preceded publication")?,
            );
            work
        } else {
            reliable(
                driver,
                link.mac_key,
                &link.mac,
                None,
                "GET",
                "/v1/work?wait_s=0",
                vec![],
                200,
            )
            .await?
        };
        Ok::<_, anyhow::Error>(work)
    };
    let work = if let Some(deadline) = delivery_deadline {
        wakeup_bound(deadline, submit_and_receive).await?
    } else {
        submit_and_receive.await?
    };
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
async fn bounded_pressure<T, F>(deadline: Instant, operation: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::time::timeout_at(deadline.into(), operation)
        .await
        .context("pressure budget exhausted; recovery reserved")?
}
async fn join_pressure(jobs: &mut tokio::task::JoinSet<()>, deadline: Instant) -> Result<()> {
    let result = tokio::time::timeout_at(deadline.into(), async {
        while let Some(result) = jobs.join_next().await {
            result?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    if !matches!(&result, Ok(Ok(()))) {
        jobs.abort_all();
        while jobs.join_next().await.is_some() {} // join every cancelled lane
    }
    result.context("burst pressure budget exhausted; lanes cancelled and joined")?
}

async fn burst(
    current: Current,
    links: Arc<Vec<Link>>,
    count: usize,
    counters: Arc<Mutex<Counters>>,
    origin: Instant,
    deadline: Instant,
    started: Arc<std::sync::atomic::AtomicU64>,
) -> Result<()> {
    let mut jobs = tokio::task::JoinSet::new();
    for lane in 0..64 {
        let (current, links, counters) = (current.clone(), links.clone(), counters.clone());
        let started = started.clone();
        jobs.spawn(async move {
            for index in (lane..count).step_by(64) {
                let slot = index % links.len();
                let link = &links[if slot == 3 { 2 } else { slot }];
                let d = get(&current).await;
                let start = Instant::now();
                started.fetch_add(1, Ordering::Relaxed);
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
                counters
                    .lock()
                    .unwrap()
                    .record_response(link.owner, &response, start, origin, false, false);
            }
        });
    }
    join_pressure(&mut jobs, deadline).await
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
fn validate_waiters(epoch: &[u8; 16], bytes: &[u8]) -> Result<Value> {
    ensure!(bytes.len() <= 4096, "waiter sample too large");
    let value: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        value["schema"] == 1
            && value["scope"] == "lab_pending_futures"
            && value["epoch"] == B64.encode(epoch),
        "waiter sample belongs to another epoch/schema"
    );
    ensure!(
        value["sequence"].as_u64().is_some() && value["uptime_ms"].as_u64().is_some(),
        "waiter sample lacks clock"
    );
    for route in ["work", "changes"] {
        ensure!(
            value[route]["threshold"] == 200,
            "waiter threshold mismatch"
        );
        for field in [
            "current",
            "peak",
            "entered",
            "exited",
            "above_ms",
            "longest_above_ms",
        ] {
            ensure!(
                value[route][field].as_u64().is_some(),
                "invalid waiter counter"
            );
        }
    }
    Ok(value)
}
async fn fresh_waiters(epoch: &[u8; 16]) -> Result<Value> {
    fresh_waiters_at(
        epoch,
        Path::new("/system/waiter-metrics"),
        Duration::from_secs(3),
    )
    .await
}
async fn fresh_waiters_at(epoch: &[u8; 16], directory: &Path, budget: Duration) -> Result<Value> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("lab challenge RNG"))?;
    let nonce: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    tokio::time::timeout(budget, async {
        let first = validate_waiters(
            epoch,
            &tokio::fs::read(directory.join("waiters.json")).await?,
        )?;
        let path = directory.to_path_buf();
        let request = nonce.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let temporary = path.join(format!("challenge-{request}.tmp"));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(request.as_bytes())?;
            std::fs::rename(temporary, path.join("challenge"))?;
            Ok(())
        })
        .await??;
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let path = directory.join("waiters.json");
            let bytes = tokio::fs::read(path).await?;
            let sample = validate_waiters(epoch, &bytes)?;
            if sample["challenge"] == nonce {
                ensure!(
                    sample["sequence"].as_u64() > first["sequence"].as_u64()
                        && sample["uptime_ms"].as_u64() >= first["uptime_ms"].as_u64(),
                    "fresh waiter counters reset"
                );
                return Ok::<_, anyhow::Error>(sample);
            }
        }
    })
    .await?
}

async fn parked_delivery(driver: Arc<Driver>, links: &[Link], accounts: usize) -> Result<Value> {
    let before = fresh_waiters(&driver.epoch).await?;
    ensure!(
        before["work"]["current"] == 0,
        "parked delivery phase is not isolated"
    );
    let mut consumers = Vec::new();
    for account in 0..accounts {
        let d = driver.clone();
        let link = links[account * 4].clone();
        let opened = Instant::now();
        consumers.push((
            opened,
            tokio::spawn(async move {
                let (status, body) = d
                    .call_epoch(
                        &d.epoch,
                        link.mac_key,
                        &link.mac,
                        None,
                        "GET",
                        "/v1/work?wait_s=25",
                        vec![],
                    )
                    .await?;
                Ok::<_, anyhow::Error>((status, body, Instant::now()))
            }),
        ));
    }
    let barrier = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let sample = fresh_waiters(&driver.epoch).await?;
            ensure!(
                consumers.iter().all(|(_, job)| !job.is_finished()),
                "consumer returned before encrypted publication"
            );
            if sample["work"]["current"].as_u64() == Some(accounts as u64) {
                return Ok::<_, anyhow::Error>(sample);
            }
        }
    })
    .await;
    let barrier = match barrier {
        Ok(Ok(value)) => value,
        other => {
            for (_, job) in consumers {
                job.abort();
                let _ = job.await;
            }
            return Err(anyhow::anyhow!(
                "parked consumer server barrier failed: {}",
                if other.is_err() {
                    "deadline"
                } else {
                    "observation"
                }
            ));
        }
    };
    let start = Instant::now();
    let delivery_latency = Arc::new(Mutex::new(Latency::default()));
    let mut transfers = Vec::new();
    for (account, (opened, consumer)) in consumers.into_iter().enumerate() {
        let delivery_latency = delivery_latency.clone();
        let d = driver.clone();
        let link = links[account * 4].clone();
        transfers.push(tokio::spawn(async move {
            encrypted_roundtrip_on_work(
                &d,
                &link,
                &format!("system-load-parked-{account}"),
                None,
                Some((consumer, delivery_latency, opened)),
            )
            .await
        }));
    }
    let mut failed = false;
    for transfer in transfers {
        if !matches!(transfer.await, Ok(Ok(()))) {
            failed = true;
        }
    }
    ensure!(
        !failed,
        "encrypted delivery to already parked consumers failed"
    );
    Ok(
        json!({"consumers":accounts,"server_barrier":barrier,"encrypted_roundtrips":accounts,
        "elapsed_ms":start.elapsed().as_millis() as u64,"post_to_parked_get":delivery_latency.lock().unwrap().json(),"same_get_response":true}),
    )
}

fn steady_occupancy(start: &Value, end: &Value, profile: &Value) -> Result<Value> {
    ensure!(start["epoch"] == end["epoch"], "steady epoch reset");
    let duration = end["uptime_ms"]
        .as_u64()
        .context("end uptime")?
        .checked_sub(start["uptime_ms"].as_u64().context("start uptime")?)
        .context("steady clock reset")?;
    ensure!(
        end["sequence"].as_u64() > start["sequence"].as_u64() && duration > 0,
        "steady sample stale"
    );
    let above = end["changes"]["above_ms"]
        .as_u64()
        .context("end duration")?
        .checked_sub(
            start["changes"]["above_ms"]
                .as_u64()
                .context("start duration")?,
        )
        .context("duration counter reset")?;
    ensure!(above <= duration + 1, "invalid above-threshold duration");
    let continuous = end["changes"]["longest_above_ms"]
        .as_u64()
        .context("continuous duration")?;
    let gate = &profile["server_waiter_gate"];
    ensure!(
        gate == &json!({"pending_changes":200,"steady_fraction_percent":80,"continuous_ms":5000,"fresh_timeout_ms":3000}),
        "unknown server waiter thresholds"
    );
    let pass = above as u128 * 100 >= duration as u128 * 80 && continuous >= 5000;
    Ok(
        json!({"window_ms":duration,"above_threshold_ms":above,"above_threshold_percent":above as f64*100.0/duration as f64,
        "longest_continuous_ms":continuous,"threshold_passed":pass}),
    )
}

async fn recover_after_pressure<F>(steady: Result<()>, operation: F) -> (Result<()>, Result<Value>)
where
    F: std::future::Future<Output = Result<Value>>,
{
    let recovery = match tokio::time::timeout(Duration::from_secs(90), operation).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("no-restart recovery deadline")),
    };
    (steady, recovery)
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
    let parked_delivery = parked_delivery(driver.clone(), &links, accounts).await?;
    let current = Arc::new(RwLock::new(driver));
    let steady_start = fresh_waiters(&get(&current).await.epoch).await?;
    ensure!(
        steady_start["changes"]["above_ms"] == 0,
        "prelude crossed steady-only threshold"
    );
    let origin = Instant::now();
    let deadline = origin + Duration::from_secs(seconds);
    let pressure_deadline = deadline + Duration::from_secs(35);
    let stop = Arc::new(AtomicBool::new(false));
    let refresh = {
        let current = current.clone();
        let stop = stop.clone();
        tokio::spawn(bounded_pressure(pressure_deadline, async move {
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
        }))
    };
    let steady_end = {
        let epoch = get(&current).await.epoch;
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline.into()).await;
            fresh_waiters(&epoch).await
        })
    };
    let polls = Arc::new(Mutex::new(Counters::default()));
    let bursts = Arc::new(Mutex::new(Counters::default()));
    let burst_started = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let work_polls = Arc::new(Mutex::new(Counters::default()));
    let revoked_during_load = Arc::new(AtomicBool::new(false));
    let mut workers = vec![];
    for lane in 0..concurrency {
        let (current, links, polls) = (current.clone(), links.clone(), polls.clone());
        let revoked = revoked_during_load.clone();
        workers.push(tokio::spawn(bounded_pressure(
            pressure_deadline,
            async move {
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
                    polls.lock().unwrap().record_response(
                        link.owner,
                        &response,
                        start,
                        origin,
                        idle_reply,
                        lane % links.len() == 3 && revoked.load(Ordering::Relaxed),
                    );
                    tokio::time::sleep(Duration::from_millis(if matches!(code, Some(429 | 503)) {
                        250 + (lane % 17) as u64 * 13
                    } else {
                        50 + (lane % 11) as u64 * 5
                    }))
                    .await;
                }
                Ok::<_, anyhow::Error>(())
            },
        )));
    }
    for lane in 0..accounts * 2 {
        let (current, links, work_polls) = (current.clone(), links.clone(), work_polls.clone());
        workers.push(tokio::spawn(bounded_pressure(
            pressure_deadline,
            async move {
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
                    work_polls
                        .lock()
                        .unwrap()
                        .record_response(link.owner, &response, start, origin, true, false);
                    tokio::time::sleep(Duration::from_millis(if matches!(code, Some(429 | 503)) {
                        400 + (lane % 13) as u64 * 17
                    } else {
                        200 + (lane % 7) as u64 * 13
                    }))
                    .await;
                }
                Ok::<_, anyhow::Error>(())
            },
        )));
    }
    let mut crypto_jobs = vec![];
    let mixed_counts = Arc::new(Mutex::new(vec![[0u64; 2]; accounts]));
    let mixed_latency = Arc::new(Mutex::new(MixedLatency::default()));
    for account in 0..accounts {
        let (current, links) = (current.clone(), links.clone());
        let mixed_counts = mixed_counts.clone();
        let mixed_latency = mixed_latency.clone();
        crypto_jobs.push(tokio::spawn(bounded_pressure(
            pressure_deadline,
            async move {
                let mut completed = 0;
                for iteration in 0..20 {
                    if Instant::now() >= deadline {
                        break;
                    }
                    let d = get(&current).await;
                    mixed_counts.lock().unwrap()[account][0] += 1;
                    let began = Instant::now();
                    let (result, retries) = LOCAL_CRYPTO_RETRIES
                        .scope(std::cell::Cell::new(0), async {
                            let result = encrypted_roundtrip(
                                &d,
                                &links[account * 4],
                                &format!("system-load-mixed-{account}-{iteration}"),
                            )
                            .await;
                            (result, LOCAL_CRYPTO_RETRIES.with(|count| count.get()))
                        })
                        .await;
                    mixed_latency
                        .lock()
                        .unwrap()
                        .record(result.is_ok(), retries, began.elapsed());
                    result?;
                    completed += 1;
                    mixed_counts.lock().unwrap()[account][1] += 1;
                    tokio::time::sleep(Duration::from_secs((seconds / 20).max(1))).await;
                }
                Ok::<_, anyhow::Error>(completed)
            },
        )));
    }
    let revoke_job = {
        let (current, links, revoked) =
            (current.clone(), links.clone(), revoked_during_load.clone());
        tokio::spawn(bounded_pressure(pressure_deadline, async move {
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
        }))
    };
    let burst_job = {
        let burst_started = burst_started.clone();
        let (current, links, bursts) = (current.clone(), links.clone(), bursts.clone());
        tokio::spawn(bounded_pressure(
            pressure_deadline + Duration::from_secs(3),
            async move {
                tokio::time::sleep(Duration::from_secs((seconds / 4).min(30))).await;
                burst(
                    current.clone(),
                    links.clone(),
                    count,
                    bursts.clone(),
                    origin,
                    pressure_deadline,
                    burst_started.clone(),
                )
                .await?;
                tokio::time::sleep(Duration::from_secs(seconds / 2)).await;
                burst(
                    current,
                    links,
                    count,
                    bursts,
                    origin,
                    pressure_deadline,
                    burst_started,
                )
                .await
            },
        ))
    };
    let mut failures = Vec::<String>::new();
    for worker in workers {
        if !matches!(worker.await, Ok(Ok(()))) {
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
    let steady_end = match steady_end.await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("steady server sampler failed")),
    };
    let steady_occupancy = match &steady_end {
        Ok(end) => steady_occupancy(&steady_start, end, &profile),
        Err(_) => Err(anyhow::anyhow!("steady server sample unavailable")),
    };
    let mixed_by_account = mixed_counts.lock().unwrap().clone();
    let mut mixed_roundtrip_latency = mixed_latency.lock().unwrap().json();
    mixed_roundtrip_latency["cancelled_roundtrips"] = json!(
        mixed_by_account
            .iter()
            .map(|counts| counts[0] - counts[1])
            .sum::<u64>()
            .saturating_sub(
                mixed_roundtrip_latency["failed_roundtrips"]["count"]
                    .as_u64()
                    .unwrap()
            )
    );
    let mixed_roundtrips: u64 = mixed_by_account.iter().map(|counts| counts[1]).sum();
    let mixed_attempts: u64 = mixed_by_account.iter().map(|counts| counts[0]).sum();
    let poll_json = polls.lock().unwrap().json();
    let mut burst_json = bursts.lock().unwrap().json();
    let started = burst_started.load(Ordering::Relaxed);
    burst_json["nominal_requests"] = json!(count * 2);
    burst_json["started_requests"] = json!(started);
    burst_json["cancelled_requests"] = json!(started.saturating_sub(bursts.lock().unwrap().count));
    burst_json["not_started_requests"] = json!((count as u64 * 2).saturating_sub(started));
    let work_json = work_polls.lock().unwrap().json();
    save(
        "load-progress.json",
        json!({"phase":"steady_finished","polls":poll_json,"work_polls":work_json,"bursts":burst_json,
            "oidc_refresh_batches":refreshes,"mixed_roundtrips":mixed_roundtrips,
            "mixed_attempts":mixed_attempts,"mixed_roundtrip_latency":mixed_roundtrip_latency,"mixed_account_columns":["attempted","completed"],
            "mixed_by_account":mixed_by_account,"revocation_under_load_seconds":revoke_seconds,
            "mixed_overload_retries":CRYPTO_RETRIES.load(Ordering::Relaxed),"failures":failures}),
    )?;
    // Preserve the original sustained gate, but defer returning its failure
    // until post-pressure recovery has been attempted and written to artifacts.
    let sustained: Result<()> = (|| {
        ensure!(
            failures.is_empty(),
            "load workers failed; inspect load-progress.json"
        );
        revoke_seconds.context("missing revocation observation")?;
        refreshes.context("missing refresh observation")?;
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
        Ok(())
    })();
    save(
        "load-progress.json",
        json!({"phase":"recovering_without_restart","steady_passed":sustained.is_ok(),
        "polls":poll_json,"work_polls":work_json,"bursts":burst_json,"failures":failures}),
    )?;
    let (sustained,recovery) = recover_after_pressure(sustained,async {
        let next=get(&current).await.refresh().await?;
        ensure!(next.epoch==get(&current).await.epoch,"recovery unexpectedly changed epoch");
        *current.write().await=Arc::new(next);
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
    let expected=get(&current).await.epoch.clone();
    let bootstrap=get(&current).await.http.get("http://relay:18081/v1/bootstrap").send().await?.error_for_status()?.json::<Value>().await?;
    ensure!(bootstrap["epoch"]==B64.encode(expected),"relay restarted during recovery");
    Ok::<_,anyhow::Error>(json!({"all_links_readable_before_selective_revoke":true,"encrypted_roundtrips":accounts+1,
        "revoked_link_rejected":true,"other_link_preserved":true,"same_epoch":true,"existing_grant_refreshed":true}))
    }).await;
    let server_waiters = fresh_waiters(&get(&current).await.epoch).await;
    let waiter_gate = server_waiters.as_ref().is_ok_and(|v| {
        v["work"]["current"] == 0
            && v["changes"]["current"] == 0
            && v["work"]["peak"].as_u64().unwrap()
                <= profile["compiled_limits"]["work"].as_u64().unwrap()
            && v["changes"]["peak"].as_u64().unwrap()
                <= profile["compiled_limits"]["changes"].as_u64().unwrap()
            && (seconds < 600
                || profile["limits"] != "tuned"
                || steady_occupancy
                    .as_ref()
                    .is_ok_and(|v| v["threshold_passed"] == true))
    });
    save(
        "load-progress.json",
        json!({"phase":"recovery_without_restart_finished","steady_passed":sustained.is_ok(),
        "recovery_passed":recovery.is_ok(),"server_waiters_passed":waiter_gate,
        "polls":poll_json,"work_polls":work_json,"bursts":burst_json,"failures":failures}),
    )?;
    let restart = if recovery.is_ok() {
        save(
            "load-progress.json",
            json!({"phase":"idle_before_restart","steady_passed":sustained.is_ok(),
            "recovery_passed":true,"polls":poll_json,"work_polls":work_json,"bursts":burst_json}),
        )?;
        tokio::time::sleep(Duration::from_secs(15)).await;
        Some(restart_probe(get(&current).await, &links).await)
    } else {
        None
    };
    let restart_passed = matches!(&restart, Some(Ok(_)));
    let passed = sustained.is_ok()
        && recovery.is_ok()
        && steady_occupancy.is_ok()
        && waiter_gate
        && restart_passed;
    save(
        "load.json",
        json!({"phase":"load","status":if passed {"passed"} else {"failed"},"profile":profile,"accounts":accounts,"macs":accounts*2,"links":links.len(),
        "elapsed_seconds":origin.elapsed().as_secs_f64(),"polls":poll_json,"work_polls":work_json,"bursts":burst_json,"oidc_refresh_batches":refreshes,"mixed_encrypted_roundtrips":mixed_roundtrips,"mixed_overload_retries":CRYPTO_RETRIES.load(Ordering::Relaxed),"revocation_under_load_seconds":revoke_seconds,
        "steady_passed":sustained.is_ok(),"steady_failures":failures,"parked_delivery":parked_delivery,"mixed_roundtrip_latency":mixed_roundtrip_latency,
        "server_waiters_passed":waiter_gate,"server_waiters":server_waiters.ok(),
        "steady_server_start":steady_start,"steady_server_end":steady_end.ok(),"steady_occupancy":steady_occupancy.ok(),
        "recovery_passed":recovery.is_ok(),"recovery":recovery.ok(),
        "restart_passed":restart_passed,"restart":restart.and_then(Result::ok),
        "scope":"real management/Keycloak/relay protocol, synthetic Macs; no daemon/Claude/application UI"}),
    )?;
    ensure!(
        passed,
        "load/recovery evidence failed; inspect load.json (recovery never hides sustained failures)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestDirectory(std::path::PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("alondra-waiter-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, value: &Value) {
            let path = self.0.join("sample.tmp");
            std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
            std::fs::rename(path, self.0.join("waiters.json")).unwrap();
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn sample(epoch: [u8; 16]) -> Value {
        json!({"schema":1,"scope":"lab_pending_futures","epoch":B64.encode(epoch),"sequence":1,"uptime_ms":1,
            "changes":{"threshold":200,"current":0,"peak":0,"entered":0,"exited":0,"above_ms":0,"longest_above_ms":0},
            "work":{"threshold":200,"current":0,"peak":0,"entered":0,"exited":0,"above_ms":0,"longest_above_ms":0}})
    }
    #[tokio::test]
    async fn delayed_old_publication_cannot_satisfy_observation_challenge() {
        let directory = Arc::new(TestDirectory::new());
        let epoch = [9u8; 16];
        let old = sample(epoch);
        directory.write(&old);
        let publisher = directory.clone();
        let late = tokio::spawn(async move {
            while !publisher.0.join("challenge").exists() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
            let mut captured_before_request = old;
            captured_before_request["sequence"] = json!(2);
            captured_before_request["uptime_ms"] = json!(2);
            publisher.write(&captured_before_request);
        });
        let error = fresh_waiters_at(&epoch, &directory.0, Duration::from_millis(100))
            .await
            .unwrap_err();
        late.await.unwrap();
        assert!(
            error
                .downcast_ref::<tokio::time::error::Elapsed>()
                .is_some(),
            "an advancing old publication must time out"
        );
        let previous = std::fs::read_to_string(directory.0.join("challenge")).unwrap();
        let publisher = directory.clone();
        let fresh = tokio::spawn(async move {
            loop {
                let nonce = std::fs::read_to_string(publisher.0.join("challenge")).unwrap();
                if nonce != previous {
                    let mut captured = sample(epoch);
                    captured["sequence"] = json!(3);
                    captured["uptime_ms"] = json!(3);
                    captured["challenge"] = json!(nonce);
                    publisher.write(&captured);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });
        assert_eq!(
            fresh_waiters_at(&epoch, &directory.0, Duration::from_secs(1))
                .await
                .unwrap()["sequence"],
            3
        );
        fresh.await.unwrap();
    }
    #[tokio::test]
    async fn suppressed_notification_timer_recheck_fails_wakeup_bound() {
        let notify = Arc::new(tokio::sync::Notify::new());
        let n = notify.clone();
        // The same waiting future eventually returns queued work after its timer,
        // exactly the false positive the old wait25/collect30 contract allowed.
        let mut original = tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_millis(180), n.notified()).await;
            true
        });
        let result = wakeup_bound(Instant::now() + Duration::from_millis(40), async {
            Ok((&mut original).await?)
        })
        .await;
        assert!(result.is_err());
        assert!(
            original.await.unwrap(),
            "timer fallback would have passed the old check"
        );
        let n = notify.clone();
        let mut prompt = tokio::spawn(async move {
            n.notified().await;
            true
        });
        notify.notify_one();
        assert!(
            wakeup_bound(Instant::now() + Duration::from_secs(1), async {
                Ok((&mut prompt).await?)
            })
            .await
            .unwrap()
        );
    }
    #[tokio::test]
    async fn stalled_burst_lanes_are_cancelled_joined_and_recovery_runs() {
        struct Running(Arc<std::sync::atomic::AtomicU64>);
        impl Drop for Running {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let active = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let active = active.clone();
            jobs.spawn(async move {
                active.fetch_add(1, Ordering::SeqCst);
                let _guard = Running(active);
                std::future::pending::<()>().await;
            });
        }
        while active.load(Ordering::SeqCst) != 8 {
            tokio::task::yield_now().await;
        }
        let steady = tokio::time::timeout(
            Duration::from_secs(1),
            join_pressure(&mut jobs, Instant::now() + Duration::from_millis(40)),
        )
        .await
        .unwrap();
        assert!(steady.is_err());
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(jobs.is_empty());
        let (steady, recovery) =
            recover_after_pressure(steady, async { Ok(json!({"probe":"after_cancel"})) }).await;
        assert!(steady.is_err());
        assert_eq!(recovery.unwrap()["probe"], "after_cancel");
    }
    #[tokio::test]
    async fn sustained_failure_still_executes_recovery_and_remains_failure() {
        let completed = Arc::new(AtomicBool::new(false));
        let observed = completed.clone();
        let (steady, recovery) = recover_after_pressure(
            Err(anyhow::anyhow!("injected sustained failure")),
            async move {
                tokio::task::yield_now().await;
                observed.store(true, Ordering::SeqCst);
                Ok(json!({"probe":"completed"}))
            },
        )
        .await;
        assert!(completed.load(Ordering::SeqCst));
        assert!(steady.is_err());
        assert_eq!(recovery.unwrap()["probe"], "completed");
    }
    #[test]
    fn server_occupancy_requires_eighty_percent_and_continuity_not_a_peak() {
        let profile = json!({"server_waiter_gate":{"pending_changes":200,"steady_fraction_percent":80,"continuous_ms":5000,"fresh_timeout_ms":3000}});
        let start =
            json!({"epoch":"synthetic","sequence":1,"uptime_ms":100,"changes":{"above_ms":0}});
        let mut end = json!({"epoch":"synthetic","sequence":2,"uptime_ms":600100,"changes":{"above_ms":480000,"longest_above_ms":5000}});
        assert_eq!(
            steady_occupancy(&start, &end, &profile).unwrap()["threshold_passed"],
            true
        );
        end["changes"]["above_ms"] = json!(479999);
        assert_eq!(
            steady_occupancy(&start, &end, &profile).unwrap()["threshold_passed"],
            false
        );
        end["changes"]["above_ms"] = json!(500000);
        end["changes"]["longest_above_ms"] = json!(4999);
        assert_eq!(
            steady_occupancy(&start, &end, &profile).unwrap()["threshold_passed"],
            false
        );
        end["epoch"] = json!("reset");
        assert!(steady_occupancy(&start, &end, &profile).is_err());
    }
    #[test]
    fn mixed_success_latency_includes_retries_but_never_failed_work() {
        let mut latency = MixedLatency::default();
        for _ in 0..99 {
            latency.record(true, 0, Duration::from_millis(10));
        }
        latency.record(true, 3, Duration::from_millis(1000));
        latency.record(false, 0, Duration::from_millis(60000));
        let result = latency.json();
        assert_eq!(result["successful_roundtrips"]["count"], 100);
        assert_eq!(result["successful_roundtrips"]["p99_ms"], 10);
        assert_eq!(result["successful_after_overload_subset"]["p95_ms"], 1000);
        assert_eq!(result["failed_roundtrips"]["p95_ms"], 60000);
    }
    #[test]
    fn waiter_evidence_rejects_another_epoch_and_missing_numeric_counters() {
        let epoch = [7u8; 16];
        let sample = json!({"schema":1,"scope":"lab_pending_futures","epoch":B64.encode(epoch),"sequence":1,"uptime_ms":20,
            "work":{"current":1,"peak":2,"entered":4,"exited":3,"above_ms":0,"longest_above_ms":0,"threshold":200},"changes":{"current":0,"peak":4,"entered":8,"exited":8,"above_ms":0,"longest_above_ms":0,"threshold":200}});
        assert!(validate_waiters(&epoch, &serde_json::to_vec(&sample).unwrap()).is_ok());
        assert!(validate_waiters(&[8u8; 16], &serde_json::to_vec(&sample).unwrap()).is_err());
        let mut broken = sample;
        broken["work"]["current"] = json!("1");
        assert!(validate_waiters(&epoch, &serde_json::to_vec(&broken).unwrap()).is_err());
    }
    #[test]
    fn diagnostic_codes_are_allowlisted_and_bucketed_without_body_or_url() {
        let response = Ok((
            503,
            json!({"error":"account_access_unavailable", "secret":"do-not-record"}),
        ));
        let mut counters = Counters::default();
        let now = Instant::now();
        counters.record_response(0, &response, now, now, false, false);
        let report = counters.json();
        assert_eq!(
            report["failure_categories"]["http_503_account_access_unavailable"],
            1
        );
        assert_eq!(
            report["failure_buckets_30s"]["0"]["http_503_account_access_unavailable"],
            1
        );
        assert!(!report.to_string().contains("do-not-record"));
        assert_eq!(
            failure_category(&Ok((503, json!({"error":"secret-provider-body"})))),
            Some("http_503_unrecognized_code".into())
        );
        assert_eq!(
            failure_category(&Err(anyhow::anyhow!(
                "https://secret.example/?token=secret"
            ))),
            Some("transport_other".into())
        );
    }

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
