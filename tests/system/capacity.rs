//! Mixed capacity driver. Unique synthetic identities, shared account grants, actual Pending gauges.
//! Does not replace encrypted mixed-work, native, or public-network regression suites.
use super::*;
use std::collections::BTreeMap;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::RwLock;

type Current = Arc<RwLock<Arc<Driver>>>;
#[derive(Clone)]
struct Endpoint {
    id: String,
    signing: SigningKey,
    owner: usize,
    mac: usize,
    is_mac: bool,
}
fn signing(index: u32) -> SigningKey {
    // Small nonzero scalar, unique per fixture endpoint; never a production key.
    let mut bytes = [0u8; 32];
    bytes[24..28].copy_from_slice(b"CAPA");
    bytes[28..].copy_from_slice(&(index + 1).to_be_bytes());
    SigningKey::from_slice(&bytes).unwrap()
}
fn endpoints(total: usize) -> Vec<Endpoint> {
    let macs = total / 3;
    let mut out = Vec::with_capacity(total);
    for i in 0..macs {
        out.push(Endpoint {
            id: format!("capacity-mac-{i}"),
            signing: signing(i as u32),
            owner: i % 16,
            mac: i,
            is_mac: true,
        });
    }
    for i in 0..macs {
        for child in 0..(2 + usize::from(i < total % 3)) {
            let n = out.len();
            out.push(Endpoint {
                id: format!("capacity-client-{i}-{child}"),
                signing: signing(n as u32),
                owner: i % 16,
                mac: i,
                is_mac: false,
            });
        }
    }
    out
}
async fn current(shared: &Current) -> Arc<Driver> {
    shared.read().await.clone()
}
async fn call(driver: &Driver, e: &Endpoint, path: &str) -> Result<(u16, Value)> {
    driver
        .call_signed(
            &driver.epoch,
            &e.signing,
            &e.id,
            if e.is_mac { None } else { Some(e.owner) },
            "GET",
            path,
            vec![],
        )
        .await
}
#[derive(Default)]
struct Counts {
    offered: u64,
    controlled_cancelled: u64,
    attempted: u64,
    accepted: u64,
    errors: u64,
    holds: u64,
    statuses: BTreeMap<u16, u64>,
    categories: BTreeMap<String, u64>,
    histogram: BTreeMap<u64, u64>,
    error_histogram: BTreeMap<u64, u64>,
    accounts: BTreeMap<usize, u64>,
}
impl Counts {
    fn record(
        &mut self,
        owner: usize,
        response: &Result<(u16, Value)>,
        elapsed: Duration,
        long: bool,
    ) {
        self.attempted += 1;
        let ms = elapsed.as_millis().min(60_000) as u64;
        if matches!(response, Ok((200 | 204, _))) {
            *self.histogram.entry(ms).or_default() += 1;
        } else {
            *self.error_histogram.entry(ms).or_default() += 1;
        }
        match response {
            Ok((code, _)) => {
                *self.statuses.entry(*code).or_default() += 1;
                if matches!(code, 200 | 204) {
                    self.accepted += 1;
                    *self.accounts.entry(owner).or_default() += 1;
                    if long && ms >= 19_000 {
                        self.holds += 1;
                    }
                } else {
                    self.errors += 1;
                    let category = match code {
                        401 => "unauthorized",
                        429 => "overload_429",
                        503 => "unavailable_503",
                        _ => "unexpected_http",
                    };
                    *self.categories.entry(category.into()).or_default() += 1;
                }
            }
            Err(error) => {
                self.errors += 1;
                let category = if error
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(|e| e.is_timeout())
                {
                    "transport_timeout"
                } else {
                    "transport_or_protocol"
                };
                *self.categories.entry(category.into()).or_default() += 1;
            }
        }
    }
    fn json(&self) -> Value {
        let p = |percent: u64| {
            let target = (self.accepted * percent).div_ceil(100);
            let mut sum = 0;
            for (ms, n) in &self.histogram {
                sum += n;
                if sum >= target {
                    return *ms;
                }
            }
            0
        };
        json!({"offered_requests":self.offered,"completed_attempts":self.attempted,"controlled_cancelled":self.controlled_cancelled,"aborted_or_inflight":self.offered.saturating_sub(self.attempted+self.controlled_cancelled),"accepted":self.accepted,"unexpected_errors":self.errors,"completed_idle_holds_19s":self.holds,"statuses":self.statuses,"error_categories":self.categories,"p50_ms":p(50),"p95_ms":p(95),"p99_ms":p(99),"accepted_latency_histogram_ms":self.histogram,"error_latency_histogram_ms":self.error_histogram,"accepted_per_account":self.accounts})
    }
}
fn short_latency(bursts: &Value) -> Result<Value> {
    let p95 = bursts["p95_ms"].as_u64().context("short p95 missing")?;
    let p99 = bursts["p99_ms"].as_u64().context("short p99 missing")?;
    let accepted = bursts["accepted"]
        .as_u64()
        .context("accepted burst count missing")?;
    ensure!(p95 <= p99, "invalid short latency ordering");
    Ok(
        json!({"passed":accepted == 10_000 && p95 <= 500 && p99 <= 2000,
        "accepted_samples":accepted,"p95_ms":p95,"p99_ms":p99,
        "p95_budget_ms":500,"p99_budget_ms":2000,
        "scope":"fixed internal lab regression budgets, not a commercial SLA"}),
    )
}
async fn fresh(epoch: &[u8; 16], profile: &Value) -> Result<Value> {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let dir = Path::new("/system/waiter-metrics");
    let tmp = dir.join(format!("capacity-{nonce}.tmp"));
    std::fs::write(&tmp, &nonce)?;
    std::fs::rename(tmp, dir.join("challenge"))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        ensure!(Instant::now() < deadline, "fresh capacity waiter deadline");
        let bytes = tokio::fs::read(dir.join("waiters.json")).await?;
        ensure!(bytes.len() <= 4096, "waiter sample oversized");
        let sample: Value = serde_json::from_slice(&bytes)?;
        if sample["challenge"] == nonce {
            ensure!(
                sample["schema"] == 1
                    && sample["scope"] == "lab_pending_futures"
                    && sample["epoch"] == B64.encode(epoch),
                "waiter identity mismatch"
            );
            for route in ["changes", "work", "total"] {
                ensure!(
                    sample[route]["threshold"]
                        == profile["server_waiter_gate"]["thresholds"][route],
                    "capacity threshold mismatch"
                );
                for field in [
                    "current",
                    "peak",
                    "entered",
                    "exited",
                    "above_ms",
                    "longest_above_ms",
                    "observed_uptime_ms",
                ] {
                    ensure!(
                        sample[route][field].as_u64().is_some(),
                        "missing waiter counter"
                    );
                }
                ensure!(
                    sample[route]["entered"]
                        .as_u64()
                        .unwrap()
                        .checked_sub(sample[route]["exited"].as_u64().unwrap())
                        == sample[route]["current"].as_u64(),
                    "invalid waiter balance"
                );
            }
            ensure!(
                sample["sequence"].as_u64().is_some() && sample["uptime_ms"].as_u64().is_some(),
                "missing waiter clock"
            );
            return Ok(sample);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
fn occupancy(start: &Value, end: &Value) -> Result<Value> {
    ensure!(start["epoch"] == end["epoch"], "unexpected steady restart");
    let elapsed = end["uptime_ms"]
        .as_u64()
        .context("end clock")?
        .checked_sub(start["uptime_ms"].as_u64().context("start clock")?)
        .context("clock reset")?;
    ensure!(
        elapsed >= 600_000 && end["sequence"].as_u64() > start["sequence"].as_u64(),
        "steady window shortened or stale"
    );
    let mut routes = serde_json::Map::new();
    let mut passed = true;
    for route in ["changes", "work", "total"] {
        // Each gauge captures its clock under the same lock as its counters.
        let elapsed = end[route]["observed_uptime_ms"]
            .as_u64()
            .context("route end clock")?
            .checked_sub(
                start[route]["observed_uptime_ms"]
                    .as_u64()
                    .context("route start clock")?,
            )
            .context("route clock reset")?;
        ensure!(elapsed >= 600_000, "cohort steady window shortened");
        let before = start[route]["above_ms"].as_u64().context("before")?;
        let after = end[route]["above_ms"].as_u64().context("after")?;
        let above = after.checked_sub(before).context("occupancy reset")?;
        ensure!(above <= elapsed + 2, "invalid occupancy time");
        // Conservative: do not borrow a continuous interval from preparation/ramp.
        let additional = end[route]["longest_above_ms"]
            .as_u64()
            .context("continuous")?
            .saturating_sub(
                start[route]["longest_above_ms"]
                    .as_u64()
                    .context("before continuous")?,
            );
        let ok = above as u128 * 100 >= elapsed as u128 * 80 && additional >= 5000;
        passed &= ok;
        routes.insert(route.into(),json!({"threshold":end[route]["threshold"],"window_ms":elapsed,"above_ms":above,"fraction_percent":above as f64*100./elapsed as f64,"additional_longest_ms":additional,"passed":ok}));
    }
    Ok(
        json!({"passed":passed,"cohorts":routes,"continuous_rule":"end longest minus pre-steady longest; conservative, never credits ramp"}),
    )
}
fn phase(value: &str) -> Result<()> {
    save(
        "load-progress.json",
        json!({"phase":value,"profile":"capacity"}),
    )
}
async fn setup(shared: &Current, nodes: &[Endpoint]) -> Result<()> {
    let started = Instant::now();
    let mut last_refresh = Instant::now();
    for node in nodes {
        ensure!(
            started.elapsed() < Duration::from_secs(1200),
            "capacity preparation deadline"
        );
        if last_refresh.elapsed() > Duration::from_secs(20) {
            let refreshed = current(shared).await.refresh().await?;
            *shared.write().await = Arc::new(refreshed);
            last_refresh = Instant::now();
        }
        let d = current(shared).await;
        let (signer, path, who, body) = if node.is_mac {
            (
                node,
                "/v1/macs",
                Some(node.owner),
                json!({"mac_id":node.id,"signing_key":B64.encode(node.signing.verifying_key().to_sec1_point(false).as_bytes()),"agreement_key":B64.encode(signing(100_000+node.mac as u32).verifying_key().to_sec1_point(false).as_bytes())}),
            )
        } else {
            (
                &nodes[node.mac],
                "/v1/devices",
                None,
                json!({"device_id":node.id,"owner_sub":d.fixture["owners"][node.owner]["id"],"signing_key":B64.encode(node.signing.verifying_key().to_sec1_point(false).as_bytes())}),
            )
        };
        let (status, _) = d
            .call_signed(
                &d.epoch,
                &signer.signing,
                &signer.id,
                who,
                "POST",
                path,
                serde_json::to_vec(&body)?,
            )
            .await?;
        ensure!(status == 201, "capacity identity registration rejected");
    }
    Ok(())
}
async fn epoch_refresh(shared: &Current) -> Result<()> {
    let mut next = (*current(shared).await).clone();
    let bootstrap = next
        .http
        .get("http://relay:18081/v1/bootstrap")
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    next.epoch = B64
        .decode(bootstrap["epoch"].as_str().context("epoch")?)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("epoch length"))?;
    *shared.write().await = Arc::new(next.refresh().await?);
    Ok(())
}
async fn recovery(shared: &Current, nodes: &[Endpoint], profile: &Value) -> Result<Value> {
    let next = current(shared).await.refresh().await?;
    *shared.write().await = Arc::new(next);
    phase("idle_before_restart")?;
    tokio::time::sleep(Duration::from_secs(15)).await;
    let before = fresh(&current(shared).await.epoch, profile).await?;
    ensure!(
        ["changes", "work", "total"]
            .into_iter()
            .all(|route| before[route]["current"] == 0),
        "pending waits retained after cancellation"
    );
    let began = Instant::now();
    let macs = nodes.len() / 3;
    // Sample one Mac and one client for every real account; full matrix remains registered.
    for owner in 0..16 {
        for node in [
            nodes.iter().find(|n| n.is_mac && n.owner == owner).unwrap(),
            nodes
                .iter()
                .find(|n| !n.is_mac && n.owner == owner)
                .unwrap(),
        ] {
            let path = if node.is_mac {
                "/v1/work?wait_s=0"
            } else {
                "/v1/changes?since=0&wait_s=0"
            };
            let driver = current(shared).await;
            let (status, _) = call(&driver, node, path).await?;
            ensure!(
                matches!(status, 200 | 204),
                "account did not recover before restart"
            );
        }
    }
    let victim = &nodes[macs];
    let parent = &nodes[victim.mac];
    let d = current(shared).await;
    let (status, _) = d
        .call_signed(
            &d.epoch,
            &parent.signing,
            &parent.id,
            None,
            "DELETE",
            &format!("/v1/devices/{}", victim.id),
            vec![],
        )
        .await?;
    ensure!(status == 204, "selective revoke failed");
    ensure!(
        call(&d, victim, "/v1/changes?since=0&wait_s=0").await?.0 == 401,
        "revoked identity accepted"
    );
    let survivor = &nodes[macs + 1];
    ensure!(
        call(&d, survivor, "/v1/changes?since=0&wait_s=0").await?.0 == 200,
        "sibling revoke leaked"
    );
    let old_epoch = d.epoch;
    save(
        "load-restart-request.json",
        json!({"operation":"restart_disposable_relay"}),
    )?;
    let until = Instant::now() + Duration::from_secs(60);
    while !Path::new("/system/load-restart-complete.json").exists() {
        ensure!(Instant::now() < until, "restart supervisor deadline");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    epoch_refresh(shared).await?;
    let next = current(shared).await;
    ensure!(next.epoch != old_epoch, "restart did not rotate epoch");
    ensure!(
        call(&next, victim, "/v1/changes?since=0&wait_s=0").await?.0 == 401,
        "revocation lost after restart"
    );
    for owner in 0..16 {
        let node = nodes
            .iter()
            .find(|n| !n.is_mac && n.owner == owner && n.id != victim.id)
            .unwrap();
        ensure!(
            call(&next, node, "/v1/changes?since=0&wait_s=0").await?.0 == 200,
            "account did not recover after restart"
        );
    }
    phase("post_restart")?;
    tokio::time::sleep(Duration::from_secs(15)).await;
    let after = fresh(&next.epoch, profile).await?;
    ensure!(
        after["total"]["current"] == 0,
        "restart idle waits retained"
    );
    Ok(
        json!({"passed":true,"idle_before_restart":before,"idle_after_restart":after,"accounts_checked":16,"revoked_rejected_before_after":true,"sibling_preserved":true,"epoch_rotated":true,"elapsed_ms":began.elapsed().as_millis(),"scope":"idle restart after pressure; does not model simultaneous full-fleet reconnect or volatile work resubmission"}),
    )
}
pub async fn entry() -> Result<()> {
    let profile: Value = serde_json::from_slice(&std::fs::read("/system/load-profile.json")?)?;
    let total = profile["connections"]
        .as_u64()
        .context("capacity connections")? as usize;
    ensure!(
        [1000, 5000, 10000].contains(&total)
            && profile["kind"] == "capacity"
            && profile["duration_seconds"] == 600
            && profile["short_latency_budget_ms"] == json!({"p95":500,"p99":2000}),
        "fixed capacity profile required"
    );
    let begin = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(2250), run(profile.clone(), total)).await;
    let result = match result {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("capacity total deadline")),
    };
    if result.is_err() && !Path::new("/system/load.json").exists() {
        let progress = std::fs::read("/system/load-progress.json")
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok());
        save(
            "load.json",
            json!({"phase":"capacity","status":"failed","profile":profile,"last_progress":progress,"elapsed_seconds":begin.elapsed().as_secs(),"failure":"capacity_preparation_or_driver_failed","recovery_passed":false}),
        )?;
    }
    result
}
async fn run(profile: Value, total: usize) -> Result<()> {
    phase("preparation")?;
    let prepared = Instant::now();
    let driver = Arc::new(Driver::open().await?);
    ensure!(
        driver.tokens.len() == 16,
        "sixteen real account grants required"
    );
    let shared = Arc::new(RwLock::new(driver));
    let nodes = Arc::new(endpoints(total));
    tokio::time::timeout(Duration::from_secs(1200), setup(&shared, &nodes))
        .await
        .context("capacity preparation deadline")??;
    let preparation_ms = prepared.elapsed().as_millis();
    let stop = Arc::new(AtomicBool::new(false));
    let measuring = Arc::new(AtomicBool::new(false));
    let reconnect = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicU64::new(0));
    let successful_reopens = Arc::new(AtomicU64::new(0));
    let polls = Arc::new(Mutex::new(Counts::default()));
    let work = Arc::new(Mutex::new(Counts::default()));
    let bursts = Arc::new(Mutex::new(Counts::default()));
    let refresh = {
        let shared = shared.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let mut count = 0;
            while !stop.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let next = current(&shared).await.refresh().await?;
                *shared.write().await = Arc::new(next);
                count += 1;
            }
            Ok::<_, anyhow::Error>(count)
        })
    };
    phase("ramp")?;
    let ramp = Instant::now();
    let mut workers = tokio::task::JoinSet::new();
    for (index, node) in nodes.iter().cloned().enumerate() {
        let (shared, stop, measuring, reconnect, cancelled) = (
            shared.clone(),
            stop.clone(),
            measuring.clone(),
            reconnect.clone(),
            cancelled.clone(),
        );
        let successful_reopens = successful_reopens.clone();
        let counts = if node.is_mac {
            work.clone()
        } else {
            polls.clone()
        };
        workers.spawn(async move {
            let mut cursor = 0;
            let mut reopened = false;
            let mut cancelled_once = false;
            while !stop.load(Ordering::Relaxed) {
                let d = current(&shared).await;
                let path = if node.is_mac {
                    "/v1/work?wait_s=20".to_owned()
                } else {
                    format!("/v1/changes?since={cursor}&wait_s=20")
                };
                let started = Instant::now();
                let in_steady = measuring.load(Ordering::Relaxed);
                if in_steady {
                    counts.lock().unwrap().offered += 1;
                }
                let response = if index % 20 == 0 && !reopened && reconnect.load(Ordering::Relaxed)
                {
                    reopened = true;
                    match tokio::time::timeout(Duration::from_secs(1), call(&d, &node, &path)).await
                    {
                        Ok(value) => value,
                        Err(_) => {
                            cancelled.fetch_add(1, Ordering::Relaxed);
                            if in_steady {
                                counts.lock().unwrap().controlled_cancelled += 1;
                            }
                            cancelled_once = true;
                            continue;
                        }
                    }
                } else {
                    call(&d, &node, &path).await
                };
                if cancelled_once && matches!(&response, Ok((200 | 204, _))) {
                    successful_reopens.fetch_add(1, Ordering::Relaxed);
                    cancelled_once = false;
                }
                if let Ok((200, value)) = &response {
                    cursor = value["revision"].as_u64().unwrap_or(cursor);
                }
                if in_steady {
                    counts
                        .lock()
                        .unwrap()
                        .record(node.owner, &response, started.elapsed(), true);
                }
                if !matches!(&response, Ok((200 | 204, _))) {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        });
        tokio::time::sleep_until((ramp + Duration::from_millis(((index + 1) * 10) as u64)).into())
            .await;
    }
    let ramp_end = Instant::now() + Duration::from_secs(30);
    let ramp_result: Result<Value> = async {
        loop {
            let sample = fresh(&current(&shared).await.epoch, &profile).await?;
            if ["changes", "work", "total"]
                .into_iter()
                .all(|r| sample[r]["current"].as_u64() >= sample[r]["threshold"].as_u64())
            {
                return Ok(sample);
            }
            ensure!(
                Instant::now() < ramp_end,
                "capacity ramp occupancy not reached"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    .await;
    let start = match ramp_result {
        Ok(start) => start,
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            workers.abort_all();
            while workers.join_next().await.is_some() {}
            let _ = refresh.await;
            let recovered = tokio::time::timeout(
                Duration::from_secs(120),
                recovery(&shared, &nodes, &profile),
            )
            .await;
            save(
                "load.json",
                json!({"phase":"capacity","status":"failed","reason":"ramp_occupancy_or_observation_failed","profile":profile,"preparation_ms":preparation_ms,"recovery_passed":matches!(&recovered,Ok(Ok(_))),"recovery":match recovered{Ok(Ok(value))=>value,_=>json!({"passed":false})}}),
            )?;
            anyhow::bail!("capacity ramp failed; recovery cannot change its verdict");
        }
    };
    let ramp_ms = ramp.elapsed().as_millis();
    phase("steady")?;
    measuring.store(true, Ordering::Relaxed);
    let steady = Instant::now();
    let mut burst_task = {
        let (shared, nodes, bursts) = (shared.clone(), nodes.clone(), bursts.clone());
        tokio::spawn(async move {
            for offset in [60, 360] {
                tokio::time::sleep_until((steady + Duration::from_secs(offset)).into()).await;
                let mut jobs = tokio::task::JoinSet::new();
                for lane in 0..64 {
                    let (shared, nodes, bursts) = (shared.clone(), nodes.clone(), bursts.clone());
                    jobs.spawn(async move {
                        for index in (lane..5000).step_by(64) {
                            let node =
                                &nodes[nodes.len() / 3 + index % (nodes.len() - nodes.len() / 3)];
                            let began = Instant::now();
                            bursts.lock().unwrap().offered += 1;
                            let driver = current(&shared).await;
                            let response =
                                call(&driver, node, "/v1/changes?since=0&wait_s=0").await;
                            bursts.lock().unwrap().record(
                                node.owner,
                                &response,
                                began.elapsed(),
                                false,
                            );
                        }
                    });
                }
                while let Some(job) = jobs.join_next().await {
                    job?;
                }
            }
            Ok::<_, anyhow::Error>(())
        })
    };
    tokio::time::sleep_until((steady + Duration::from_secs(300)).into()).await;
    reconnect.store(true, Ordering::Relaxed);
    tokio::time::sleep_until((steady + Duration::from_secs(600)).into()).await;
    let bursts_completed_in_steady = burst_task.is_finished();
    let end = fresh(&current(&shared).await.epoch, &profile).await;
    measuring.store(false, Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    workers.abort_all();
    let mut worker_panics = 0;
    while let Some(result) = workers.join_next().await {
        if result.is_err_and(|e| !e.is_cancelled()) {
            worker_panics += 1;
        }
    }
    let refresh_result = refresh.await;
    let burst_result = tokio::time::timeout(Duration::from_secs(35), &mut burst_task).await;
    if burst_result.is_err() {
        burst_task.abort();
        let _ = burst_task.await;
    }
    let occupancy = end.and_then(|end| occupancy(&start, &end));
    let poll_json = polls.lock().unwrap().json();
    let work_json = work.lock().unwrap().json();
    let burst_json = bursts.lock().unwrap().json();
    let latency = short_latency(&burst_json)?;
    let refresh_ok = matches!(&refresh_result,Ok(Ok(count)) if *count>=19);
    let steady_ok = occupancy.as_ref().is_ok_and(|o| o["passed"] == true)
        && latency["passed"] == true
        && refresh_ok
        && matches!(burst_result, Ok(Ok(Ok(()))))
        && poll_json["unexpected_errors"] == 0
        && work_json["unexpected_errors"] == 0
        && burst_json["unexpected_errors"] == 0
        && burst_json["accepted"] == 10000
        && bursts_completed_in_steady
        && poll_json["completed_idle_holds_19s"].as_u64().unwrap_or(0)
            >= (profile["clients"].as_u64().unwrap() * 20)
        && work_json["completed_idle_holds_19s"].as_u64().unwrap_or(0)
            >= (profile["macs"].as_u64().unwrap() * 20)
        && worker_panics == 0
        && cancelled.load(Ordering::Relaxed) == (total / 20) as u64
        && successful_reopens.load(Ordering::Relaxed) == (total / 20) as u64;
    let steady_result = json!({"phase":"capacity","profile":profile,"steady_passed":steady_ok,"preparation_ms":preparation_ms,"ramp_ms":ramp_ms,"occupancy":occupancy.ok(),"polls":poll_json,"work_polls":work_json,"bursts":burst_json,"short_latency":latency,"oidc_refresh_passed":refresh_ok,"controlled_cancel_count":cancelled.load(Ordering::Relaxed),"successful_reopen_count":successful_reopens.load(Ordering::Relaxed),"poll_worker_panics":worker_panics,"bursts_completed_in_steady":bursts_completed_in_steady});
    save("load-steady.json", steady_result.clone())?;
    let recovered = tokio::time::timeout(
        Duration::from_secs(120),
        recovery(&shared, &nodes, &profile),
    )
    .await;
    let recovery_ok = matches!(&recovered, Ok(Ok(_)));
    let passed = steady_ok && recovery_ok;
    let mut result = steady_result;
    result["status"] = json!(if passed { "passed" } else { "failed" });
    result["recovery_passed"] = json!(recovery_ok);
    result["recovery"] = match recovered {
        Ok(Ok(v)) => v,
        _ => json!({"passed":false,"error":"bounded_recovery_failed"}),
    };
    result["unsupported"] = json!([
        "encrypted business work mixed into this capacity window",
        "simultaneous full-fleet restart reconnect",
        "real daemon/Claude/APNs",
        "public TLS/proxy/network capacity"
    ]);
    save("load.json", result)?;
    ensure!(passed, "capacity steady/recovery gate failed");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample(clock: u64, above: u64, longest: u64) -> Value {
        let mut result = json!({"epoch":"synthetic","sequence":clock,"uptime_ms":clock});
        for route in ["changes", "work", "total"] {
            result[route] = json!({"observed_uptime_ms":clock,"above_ms":above,"longest_above_ms":longest,"threshold":800});
        }
        result
    }
    #[test]
    fn complete_error_free_bursts_still_fail_when_the_tail_is_slow() {
        let mut bursts = json!({"accepted":10000,"unexpected_errors":0,"completed_attempts":10000,
                              "offered_requests":10000,"p95_ms":500,"p99_ms":2000});
        assert_eq!(short_latency(&bursts).unwrap()["passed"], true);
        bursts["p99_ms"] = json!(29_000);
        assert_eq!(short_latency(&bursts).unwrap()["passed"], false);
        bursts["p99_ms"] = json!(2000);
        bursts["p95_ms"] = json!(501);
        assert_eq!(short_latency(&bursts).unwrap()["passed"], false);
        bursts["p95_ms"] = json!(500);
        bursts["accepted"] = json!(9999);
        assert_eq!(short_latency(&bursts).unwrap()["passed"], false);
    }
    #[test]
    fn occupancy_cannot_hide_a_starved_cohort_or_credit_ramp() {
        let start = sample(100_000, 90_000, 90_000);
        let mut end = sample(700_000, 570_000, 95_000);
        assert_eq!(occupancy(&start, &end).unwrap()["passed"], true);
        for route in ["changes", "work", "total"] {
            end[route]["above_ms"] = json!(569_999);
            assert_eq!(occupancy(&start, &end).unwrap()["passed"], false);
            end[route]["above_ms"] = json!(570_000);
        }
        end["total"]["longest_above_ms"] = json!(90_000);
        assert_eq!(occupancy(&start, &end).unwrap()["passed"], false);
        end["epoch"] = json!("restarted");
        assert!(occupancy(&start, &end).is_err());
    }
    #[test]
    fn overload_and_cancellation_remain_separate_from_accepted_latency() {
        let mut counts = Counts::default();
        counts.offered = 4;
        counts.controlled_cancelled = 1;
        counts.record(0, &Ok((200, Value::Null)), Duration::from_secs(20), true);
        counts.record(1, &Ok((429, Value::Null)), Duration::from_millis(1), true);
        let evidence = counts.json();
        assert_eq!(evidence["accepted"], 1);
        assert_eq!(evidence["p95_ms"], 20_000);
        assert_eq!(evidence["error_latency_histogram_ms"]["1"], 1);
        assert_eq!(evidence["unexpected_errors"], 1);
        assert_eq!(evidence["error_categories"]["overload_429"], 1);
        assert_eq!(evidence["controlled_cancelled"], 1);
        assert_eq!(evidence["aborted_or_inflight"], 1);
        assert_eq!(evidence["completed_idle_holds_19s"], 1);
    }
    #[test]
    fn matrix_counts_and_signing_keys_are_unique() {
        for total in [1000, 5000, 10000] {
            let nodes = endpoints(total);
            assert_eq!(nodes.len(), total);
            assert_eq!(nodes.iter().filter(|e| e.is_mac).count(), total / 3);
            let ids: std::collections::BTreeSet<_> = nodes.iter().map(|e| &e.id).collect();
            assert_eq!(ids.len(), total);
            let keys: std::collections::BTreeSet<_> = nodes
                .iter()
                .map(|e| e.signing.to_bytes().to_vec())
                .collect();
            assert_eq!(keys.len(), total);
        }
    }
}
