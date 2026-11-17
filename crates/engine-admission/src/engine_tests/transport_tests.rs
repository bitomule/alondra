//! The node transport meets the gate: its watch samples the Mac off the async worker, and a node the
//! user paused runs no tool.
use super::*;
use alondra_daemon_engine::domain_transport::PAUSED_BY_USER;
use alondra_daemon_engine::domain_transport::tests::*;

#[test]
fn runtime_progress_machine_watch_persists_off_the_async_worker() {
    wired();
    let fixture = Fixture::new();
    let domain = fixture.open();
    let driver = std::thread::current().id();
    let gauges = std::sync::Arc::new(std::sync::Mutex::new(Some((alondra_core::Machine {
        at: 1_700_000_000_000, memory_total: 100, memory_available: 50, memory_free_percent: Some(50),
        disk_total: 500_000_000_000, disk_available: 400_000_000_000, cpus: 8, ..Default::default()
    }, None, vec![]))));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let watcher = tokio::spawn(crate::machine_watch::run(domain.clone(), gauges, Silent));
        domain.machine_watch().sampled.notified().await;
        let threads = domain.machine_watch().sample_threads.lock().unwrap().clone();
        watcher.abort();
        assert!(watcher.await.unwrap_err().is_cancelled());
        assert!(!threads.is_empty());
        assert!(threads.iter().all(|thread| *thread != driver), "machine file sampling occupied the async worker: {threads:?}");
        assert!(std::fs::read_dir(fixture.0.join(crate::machine_state::SAMPLES_DIR)).unwrap().next().is_some());
    });
}

/// Whatever starts a turn of a node the user paused (a background command ending, a queued
/// message), it may run no tool, Alondra's included, and its turn end is not held back.
#[tokio::test]
async fn a_node_the_user_paused_runs_no_tool_and_may_end_its_turn() {
    wired();
    let fixture = Fixture::new();
    let domain = fixture.open();
    node_with_task(&domain);
    domain.gate().pause_by_user("agent-a", alondra_core::now_millis()).unwrap();
    for tool in ["Bash", "mcp__plugin_alondra_alondra__present_result"] {
        let (reply, result) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": tool}), Some(6)).await;
        result.unwrap();
        assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny", "{tool}: {reply}");
        assert_eq!(reply["hookSpecificOutput"]["permissionDecisionReason"], PAUSED_BY_USER);
    }
    assert_eq!(hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await.0, json!({}));

    domain.gate().release_by_user_message("agent-a");
    let (reply, _) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": "Bash"}), Some(6)).await;
    assert_ne!(reply["hookSpecificOutput"]["permissionDecision"], "deny", "{reply}");
    assert_eq!(hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await.0["decision"], "block");
}
