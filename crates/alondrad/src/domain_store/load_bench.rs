//! Seeds the production-shaped domain that `make load-bench` (tools/load_bench.py) measures.
use super::*;
use std::collections::BTreeSet;

const REQUESTS: usize = 20;
const TASKS: usize = 115;
const INTERACTIONS: usize = 669;
const AGENTS: usize = 97;

fn run(store: &mut DomainStore, actor: &Actor, id: String, command: Command) {
    let state = store.load().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: state.revision,
        command,
    };
    store.execute_authenticated(actor, &envelope, None).unwrap();
}

fn body(i: usize) -> String {
    "The node reports what it changed, what it measured and what is left to decide. "
        .repeat(15 + (i * 37) % 90)
}

#[test]
#[ignore = "run by make load-bench with ALONDRA_LOAD_BENCH_DB"]
fn seed_load_bench() {
    let path = std::env::var("ALONDRA_LOAD_BENCH_DB").expect("ALONDRA_LOAD_BENCH_DB");
    let nodes: usize = std::env::var("ALONDRA_LOAD_BENCH_NODES").map_or(20, |n| n.parse().unwrap());
    let mut store = DomainStore::open(Path::new(&path)).unwrap();
    seed(&mut store, nodes);
}

pub(crate) fn seed(store: &mut DomainStore, nodes: usize) {
    let user = Actor::User { user_id: "user-1".into() };
    for r in 0..REQUESTS {
        run(store, &Actor::Orchestrator { generation: 0 }, format!("request-{r}"), Command::CreateRequest {
            request_id: format!("request-{r}").into(),
            body: body(r),
            auto_merge: false, maintenance: false, message: false,
        });
        run(store, &user, format!("title-{r}"), Command::TitleRequest {
            request_id: format!("request-{r}").into(),
            title: format!("Bench request {r}"),
        });
    }
    for n in 0..nodes.max(AGENTS) {
        run(store, &Actor::System, format!("agent-{n}"), Command::RegisterAgent {
            agent_id: format!("bench-node-{n}").into(),
            name: format!("Bench node {n}"), at: 0
        });
    }
    for t in 0..TASKS {
        run(store, &Actor::Orchestrator { generation: 0 }, format!("task-{t}"), Command::CreateTask { plan_items: None, continues: None,
            task_id: format!("task-{t}").into(),
            request_id: format!("request-{}", t % REQUESTS).into(),
            title: format!("Bench task {t}"),
            agent_id: (t < nodes.max(AGENTS)).then(|| format!("bench-node-{t}").into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
    }
    for i in 0..INTERACTIONS {
        run(store, &Actor::System, format!("info-{i}"), Command::PublishInformation {
            interaction_id: format!("info-{i}").into(),
            scope: Scope::Task { task_id: format!("task-{}", i % TASKS).into() },
            recipient: Recipient::User,
            body: body(i),
            attachments: vec![],
            card: CardSpec::derived(CardKind::Know, "Read the update"),
        });
    }
    store.conn.execute("UPDATE domain_outbox SET state='handed'", []).unwrap();
    let state = store.load().unwrap();
    assert_eq!((state.tasks.len(), state.interactions.len()), (TASKS, INTERACTIONS));
}

fn distribution(samples: &[f64]) -> serde_json::Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.is_empty() { return serde_json::json!({"n": 0}); }
    let pick = |q: f64| sorted[(q * sorted.len() as f64).ceil() as usize - 1];
    serde_json::json!({"n": sorted.len(), "p50_ms": pick(0.5), "p95_ms": pick(0.95),
        "max_ms": sorted.last(), "samples_ms": samples})
}

fn process_resources() -> (f64, u64) {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
    let millis = |time: libc::timeval| time.tv_sec as f64 * 1000.0 + time.tv_usec as f64 / 1000.0;
    let rss_units = if cfg!(target_os = "macos") { 1 } else { 1024 };
    (millis(usage.ru_utime) + millis(usage.ru_stime), usage.ru_maxrss as u64 * rss_units)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in isolated signed card request measurement; no production service"]
async fn repeated_card_requests() {
    use alondra_core::views::{Client, Lang, Transport, View};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use p256::ecdsa::{SigningKey, Signature, signature::Signer};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::time::Instant;

    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let database = crate::domain_database_migration::active_path(&home);
    {
        let mut store = DomainStore::open(&database).unwrap();
        seed(&mut store, 20);
    }
    let daemon = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
    let signing = SigningKey::from_slice(&[9; 32]).unwrap();
    let auth = daemon.auth();
    let device = auth.pair(&auth.new_pairing_code(), "Request benchmark",
        &STANDARD.encode(signing.verifying_key().to_sec1_bytes())).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::domain_bootstrap::router(daemon.clone())
        .layer(axum::middleware::from_fn_with_state(auth, crate::auth::guard));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .with_graceful_shutdown(async { let _ = stopped.await; }).await
    });
    let measured: anyhow::Result<serde_json::Value> = async {
        let http = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).build()?;
        let key = "card:info-0";
        let path = "/views/card:info-0?protocol=1&client=mac&lang=en";
        let mut first = None::<View>;
        let mut times = Vec::new();
        let mut conditional = Vec::new();
        let mut decode = Vec::new();
        let mut bytes = Vec::new();
        let mut hashes = Vec::new();
        let mut unchanged = 0;
        let mut conditional_full = 0;
        let mut errors = Vec::new();
        let mut error_times = Vec::new();
        let mut successful_full = 0;
        let builds_before = daemon.client().views.build_count();
        let resources_before = process_resources();
        for index in 0..61 {
            let request_path = if index <= 30 { path.to_owned() } else {
                format!("{path}&known={}", first.as_ref().unwrap().version)
            };
            let start = Instant::now();
            let timestamp = (alondra_core::now_millis() as u64 + index).to_string();
            let message = crate::auth::signed_message("GET", &request_path, &timestamp, b"");
            let signature: Signature = signing.sign(message.as_bytes());
            let response = http.get(format!("{base}{request_path}"))
                .header("x-alondra-device", &device.id)
                .header("x-alondra-timestamp", timestamp)
                .header("x-alondra-signature", STANDARD.encode(signature.to_der().as_bytes()))
                .send().await?;
            let status = response.status();
            let body = response.bytes().await?;
            let decoded_at = Instant::now();
            let value: serde_json::Value = serde_json::from_slice(&body)?;
            if !status.is_success() {
                errors.push(json!({"status": status.as_u16(), "body": value}));
                error_times.push(start.elapsed().as_secs_f64() * 1000.0);
                anyhow::ensure!(index != 0, "first request failed: {errors:?}");
                continue;
            }
            if value["unchanged"] == true {
                anyhow::ensure!(index > 30, "unexpected unchanged response");
                anyhow::ensure!(value["data_epoch"] == first.as_ref().unwrap().data_epoch, "epoch changed");
                unchanged += 1;
            } else {
                let view: View = serde_json::from_value(value)?;
                anyhow::ensure!(view.key == key && view.view != alondra_core::views::SETTLED_VIEW, "wrong card");
                if let Some(expected) = &first {
                    anyhow::ensure!(view.sections == expected.sections && view.data_epoch == expected.data_epoch,
                        "card content changed");
                } else { first = Some(view); }
                if index > 30 { conditional_full += 1; } else { successful_full += 1; }
            }
            decode.push(decoded_at.elapsed().as_secs_f64() * 1000.0);
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if index <= 30 { times.push(elapsed); } else { conditional.push(elapsed); }
            bytes.push(body.len());
            hashes.push(Sha256::digest(&body).iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        }
        let resources_after = process_resources();
        let client = daemon.client();
        let request_builds = client.views.build_count() - builds_before;
        let (cache_entries, cache_serialized_view_bytes) = client.views.serialized_retention();
        let mut stages = Vec::new();
        let mut live_hashes = Vec::new();
        let mut node_state_since = Vec::new();
        for _ in 0..20 {
            let start = Instant::now();
            let read = client.domain.read();
            let state = std::sync::Arc::clone(read.state());
            let state_at = Instant::now();
            let live = crate::views::live::gather(&client, &state, Lang::En, &[key.to_owned()], true, false).await;
            let live_at = Instant::now();
            let mut view = client.views.render(key, &read, &live, Client::Mac, Lang::En, Transport::Loopback).map_err(|error| anyhow::anyhow!("render: {error:?}"))?;
            let render_at = Instant::now();
            view.service = Some(crate::views::this_service());
            let body = serde_json::to_vec(&view)?;
            let end = Instant::now();
            anyhow::ensure!(view.sections == first.as_ref().unwrap().sections
                && view.data_epoch == first.as_ref().unwrap().data_epoch, "stage oracle changed");
            live_hashes.push(Sha256::digest(serde_json::to_vec(&live)?).iter().map(|b| format!("{b:02x}")).collect::<String>());
            node_state_since.push(live.nodes.first().and_then(|node| node.state_since.clone()));
            stages.push(json!({"state_ms": state_at.duration_since(start).as_secs_f64()*1000.0,
                "gather_ms": live_at.duration_since(state_at).as_secs_f64()*1000.0,
                "render_ms": render_at.duration_since(live_at).as_secs_f64()*1000.0,
                "serialize_ms": end.duration_since(render_at).as_secs_f64()*1000.0,
                "total_ms": end.duration_since(start).as_secs_f64()*1000.0, "bytes": body.len()}));
        }
        anyhow::ensure!(errors.is_empty(), "request errors: {errors:?}");
        Ok(json!({"profile":"signed-loopback-card", "requests_offered":61,
            "successful_full":successful_full, "conditional_unchanged":unchanged,
            "conditional_full":conditional_full, "errors":errors, "error_times_ms":error_times,
            "fixture":{"requests":20,"tasks":115,"interactions":669,"agents":97},
            "request_cache_builds":request_builds, "stage_live_hashes":live_hashes,
            "request_process_cpu_ms": resources_after.0 - resources_before.0,
            "process_lifetime_maxrss_bytes_before_requests": resources_before.1,
            "process_lifetime_maxrss_bytes_after_requests": resources_after.1,
            "cache_entries_after_requests": cache_entries,
            "cache_serialized_view_bytes_after_requests": cache_serialized_view_bytes,
            "stage_first_node_state_since":node_state_since, "first_ms":times.first(), "warm":distribution(&times[1..]),
            "conditional":distribution(&conditional), "decode":distribution(&decode),
            "response_bytes":bytes, "response_hashes":hashes, "stages_separate_samples":stages,
            "limits":"Own TCP server and real signature/approval/route/decode. No Apple UI, direct discovery, relay RTT, hooks, provider network or global RSS. HTTP bytes cover bodies only; decode timing includes typed conversion and content assertions. CPU difference covers only the61requests afterfixture/server setup; getrusage RSS is the entire testprocess lifetime highwater includingfixtures, not a delta or retainedproductRSS. Cachebytes are serializedView JSON, not allocatorbytes/RSS. Stage samples are separate and not an additive decomposition of TCP samples. First is initial view/connection, not cold process startup. p95 has only30 samples; no p99 claim."}))
    }.await;
    let _ = stop.send(());
    server.await.unwrap().unwrap();
    drop(daemon);
    let result = measured.unwrap();
    println!("CARD_REQUEST_MEASUREMENT {result}");
}

#[tokio::test]
#[ignore = "opt-in isolated live store read trace; no production service"]
async fn trace_card_live_store_reads() {
    use alondra_core::views::Lang;
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    {
        let database = crate::domain_database_migration::active_path(&home);
        let mut store = DomainStore::open(&database).unwrap();
        seed(&mut store, 20);
    }
    let daemon = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
    let client = daemon.client();
    let state = client.domain.state().unwrap();
    let before = process_resources();
    client.domain.reads.lock().unwrap().begin_live_read_trace();
    let live = crate::views::live::gather(&client, &state, Lang::En, &["card:info-0".into()], true, false).await;
    let trace = client.domain.reads.lock().unwrap().end_live_read_trace();
    let after = process_resources();
    let mut getters = std::collections::BTreeMap::<&str, (usize, usize, usize, usize, f64)>::new();
    for read in &trace {
        let counts = getters.entry(read.getter).or_default();
        counts.0 += 1; counts.1 += read.statement_attempts; counts.2 += read.rows;
        counts.3 += usize::from(read.failed); counts.4 += read.elapsed_ms;
    }
    assert!(trace.iter().all(|read| !read.failed));
    assert_eq!(live.inbox_times.len(), INTERACTIONS);
    assert_eq!(live.start_waits.len(), TASKS - AGENTS);
    let live_output_sha256 = { use sha2::{Digest, Sha256}; Sha256::digest(serde_json::to_vec(&live).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect::<String>() };
    println!("LIVE_STORE_QUERY_MEASUREMENT {}", serde_json::json!({
        "fixture":{"requests": REQUESTS,"tasks": TASKS,"interactions": INTERACTIONS,"agents": AGENTS},
        "gather_calls":1, "queries":getters,"events":trace,
        "trace_process_cpu_ms":after.0 - before.0,
        "process_lifetime_maxrss_bytes":after.1,
        "live_output_sha256": live_output_sha256,
        "limits":"Actual targeted getter invocation/SQL-attempt/callback-row counts in a separate trace-enabled gather. EXISTS returns one SQL scalar row even when false. This is not a trace of every database statement or a decomposition of signed-request timing; per-getter elapsed time includes tracing. CPU/RSS are labeled trace/lifetime, not production retained memory. Existing repeated_card_requests runs the unchanged61signed requests with the trace sink disabled."}));
}
