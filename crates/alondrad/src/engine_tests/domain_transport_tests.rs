//! The producer transport while a client reads views and the feed through alondrad's routes.
use alondra_daemon_engine::domain_transport::tests::*;
use alondra_daemon_engine::domain_transport::*;
use alondra_core::domain::*;
use serde_json::{Value, json};
use tokio::net::UnixStream;

struct ReleaseRead(Option<std::sync::mpsc::Sender<()>>);
impl Drop for ReleaseRead {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() { let _ = release.send(()); }
    }
}

#[test]
fn runtime_progress_view_and_feed_sql_and_render_run_outside_the_async_worker() {
    let fixture = Fixture::new();
    let domain = fixture.open();
    working_agent(&domain, "isolation");
    let client = crate::client_routes::Client::open(domain.clone(), &fixture.0, Default::default());
    domain.reads.lock().unwrap().begin_live_read_trace();
    let driver = std::thread::current().id();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let response = crate::views::routes::relay(&client, "phone", "/views/home?protocol=1&client=ios&lang=en").await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let view: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(view["key"], "home");
        let response = crate::views::routes::relay_feed(&client, "phone", "/changes?protocol=1&client=ios&lang=en&wait=0").await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let feed: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(feed["cursor"].as_str().unwrap().starts_with("c1."));
        assert!(feed["status"].is_object());
        let runtime = client.read("/runtime").await.expect("runtime route");
        assert_eq!(runtime.status(), axum::http::StatusCode::OK);
    });
    let reads = domain.reads.lock().unwrap().end_live_read_trace();
    assert!(!reads.is_empty());
    assert!(reads.iter().any(|read| read.rows > 0), "the real populated store was read");
    assert!(reads.iter().all(|read| read.thread != format!("{driver:?}")), "SQLite ran on the async worker: {reads:?}");
    let builds = client.views.build_threads.lock().unwrap();
    assert!(!builds.is_empty(), "a cold projection was actually rendered");
    assert!(builds.iter().all(|thread| *thread != driver), "render ran on the async worker: {builds:?}");
}

#[test]
fn runtime_progress_a_held_view_and_hook_recording_do_not_stop_guards_or_machine_samples() {
    let fixture = Fixture::new();
    let domain = fixture.open();
    working_agent(&domain, "progress");
    let client = crate::client_routes::Client::open(domain.clone(), &fixture.0, Default::default());
    let driver = std::thread::current().id();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    let entered = std::sync::Mutex::new(Some(entered));
    domain.reads.lock().unwrap().on_live_read(move |_| {
        // A wrong-thread regression fails before the controlled block, rather than hanging.
        if let Some(entered) = entered.lock().unwrap().take() {
            assert_ne!(std::thread::current().id(), driver, "view SQL occupies the async worker");
            entered.send(()).unwrap();
            held.recv().unwrap();
        }
    });
    let (recording_entered, recording_started) = tokio::sync::oneshot::channel();
    let recording_entered = std::sync::Mutex::new(Some(recording_entered));
    domain.hook_work.on_start(move || {
        if let Some(entered) = recording_entered.lock().unwrap().take() {
            assert_ne!(std::thread::current().id(), driver, "hook recording occupies the async worker");
            entered.send(()).unwrap();
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let release = ReleaseRead(Some(release));
        let reading = tokio::spawn({
            let client = client.clone();
            async move { crate::views::routes::relay(&client, "phone", "/views/home?protocol=1&client=ios&lang=en").await }
        });
        started.await.expect("the real view must enter its controlled SQL read");
        let (mut poster, server) = UnixStream::pair().unwrap();
        let recording = tokio::spawn({
            let domain = domain.clone();
            async move { handle(server, &domain).await }
        });
        send_hook(&mut poster, json!({"session_id":"progress", "hook_event_name":"PostToolUse"})).await;
        assert_eq!(reply(&mut poster).await, json!({}));
        recording_started.await.expect("hook recording started while SQL was held");
        let (mut guard, server) = UnixStream::pair().unwrap();
        let guarding = tokio::spawn({
            let domain = domain.clone();
            async move { handle(server, &domain).await }
        });
        send_hook(&mut guard, json!({"session_id":"progress", "hook_event_name":"PreToolUse", "tool_name":"Bash", "tool_input":{"command":"alondra answer q --option yes"}})).await;
        let denied = reply(&mut guard).await;
        assert_eq!(denied["hookSpecificOutput"]["permissionDecision"], "deny");
        let gauges = std::sync::Arc::new(std::sync::Mutex::new(Some((alondra_core::Machine {
            at: 1_700_000_000_000, memory_total: 100, memory_available: 50, memory_free_percent: Some(50),
            disk_total: 500_000_000_000, disk_available: 400_000_000_000, cpus: 8,
            ..Default::default()
        }, None, vec![]))));
        let watcher = tokio::spawn(crate::machine_watch::run(domain.clone(), gauges, Silent));
        domain.live.machine_watch.sampled.notified().await;
        let threads = domain.live.machine_watch.sample_threads.lock().unwrap().clone();
        assert!(!threads.is_empty());
        assert!(threads.iter().all(|thread| *thread != driver));
        let samples = fixture.0.join(crate::machine_state::SAMPLES_DIR);
        assert!(std::fs::read_dir(samples).unwrap().next().is_some(), "machine sample was persisted while view held SQL");
        watcher.abort();
        assert!(watcher.await.unwrap_err().is_cancelled());
        drop(release);
        assert_eq!(reading.await.unwrap().status(), axum::http::StatusCode::OK);
        recording.await.unwrap().unwrap();
        guarding.await.unwrap().unwrap();
        println!("view_sql_held=true post_reply=ok pre_tool_use=deny machine_sample=persisted physical_jobs=drained");
    });
}

#[test]
fn an_answer_the_node_cannot_take_yet_waits_instead_of_being_dropped() {
    let fixture = Fixture::new();
    let domain = fixture.open();
    node_with_task(&domain);
    free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
    free_text_answer(&domain, user_actor(), "q", "Call it Atlas");
    assert_eq!(crate::domain_delivery::tell_sdk_nodes(&domain), 0, "the task has no execution to take it yet");
    assert_eq!(outbox_message(&domain, "q", "agent").state, "retryable", "the answer waits for the node");
}
