//! Opt-in measurements of signed card requests and the live store reads behind them, against a
//! domain `domain_store::load_bench` seeds.
use alondra_daemon_engine::domain_store::load_bench::*;
use alondra_daemon_engine::domain_store::*;

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
