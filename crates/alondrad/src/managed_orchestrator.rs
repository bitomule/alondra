//! Retire the background supervisor before enabling print-mode ownership. No transcript is
//! deleted. A persisted migration reservation fences legacy MCP calls while stop is in flight.
use std::path::Path;
use std::time::Duration;
use alondra_core::{now_millis, orchestrator::{self, Transition}};
use anyhow::{Result, anyhow, bail};
use crate::hub::Hub;

pub async fn prepare(hub: &Hub) -> Result<bool> {
    let result = prepare_with(hub, Path::new("claude")).await;
    if let Err(error) = &result {
        let text = error.to_string();
        hub.append_with(|v| (v.orchestrator.is_none() && v.orchestrator_migration_error.as_ref() != Some(&text))
            .then(|| alondra_core::Event::Orchestrator(Transition::MigrationBlocked { error: text })))?;
    }
    result
}

async fn prepare_with(hub: &Hub, executable: &Path) -> Result<bool> {
    let _lease = hub.orchestrator_commands.write().await;
    if hub.read(|v| v.orchestrator.is_some()) { return Ok(true); }
    let migration = hub.read(|v| v.orchestrator_migration.clone());
    let migration = if let Some(migration) = migration { migration } else {
        let reserved = hub.append_all(|v| {
            if v.orchestrator.is_some() { return Ok(vec![]); }
            let legacy: Vec<_> = v.nodes.values().filter(|n| n.is_orchestrator).collect();
            let transition = match legacy.as_slice() {
                [] => Transition::Enabled { session_id: None },
                [node] => Transition::MigrationStarted { session_id: node.session_id.clone(), pid: node.pid },
                _ => return Err("Multiple legacy orchestrators need reconciliation before migration".to_string()),
            };
            orchestrator::checked_transition(v, transition, now_millis()).map(|e| vec![e])
        })?;
        if let Err(error) = reserved { bail!("Orchestrator migration blocked: {error}"); }
        if hub.read(|v| v.orchestrator.is_some()) { return Ok(true); }
        hub.read(|v| v.orchestrator_migration.clone()).ok_or_else(|| anyhow!("migration reservation missing"))?
    };
    // Never call rm: stop retains Claude's conversation for --resume. Suppress legacy wakeup
    // paths while the CLI and watcher observe its process exit.
    let _closing = hub.closing(&migration.session_id);
    let mut command = tokio::process::Command::new(executable);
    command.args(["stop", &migration.session_id]).kill_on_drop(true)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let status = tokio::time::timeout(Duration::from_secs(30), command.status()).await
        .map_err(|_| anyhow!("Claude stop timed out; migration remains reserved"))??;
    if !status.success() {
        let process_gone = migration.pid.is_none_or(|pid| !crate::watcher::pid_alive(pid));
        let short = hub.read(|v| v.nodes.get(&migration.session_id).map(|n| n.short_id.clone()));
        if !process_gone || !confirmed_absent(executable, &migration.session_id, short.as_deref()).await {
            bail!("Claude stop failed. The legacy session is still present or its absence cannot be verified; resolve its state before retrying migration");
        }
    }
    if let Some(pid) = migration.pid {
        let gone = tokio::time::timeout(Duration::from_secs(10), async {
            while crate::watcher::pid_alive(pid) { tokio::time::sleep(Duration::from_millis(100)).await; }
        }).await;
        if gone.is_err() { bail!("Legacy orchestrator process remains alive after stop; refusing concurrent execution"); }
    }
    let enabled = hub.append_all(|v| {
        if v.orchestrator.is_some() { return Ok(vec![]); }
        if v.orchestrator_migration.as_ref() != Some(&migration) { return Err("migration reservation changed".to_string()); }
        if v.nodes.get(&migration.session_id).and_then(|n| n.pid).is_some_and(|pid| Some(pid) != migration.pid && crate::watcher::pid_alive(pid)) {
            return Err("legacy orchestrator acquired another process; migration remains blocked".to_string());
        }
        let at = now_millis();
        let stopped = orchestrator::checked_transition(v, Transition::LegacyStopped { session_id: migration.session_id.clone() }, at)?;
        let mut next = v.clone(); next.apply(v.seq + 1, at, &stopped);
        let enabled = orchestrator::checked_transition(&next, Transition::Enabled { session_id: Some(migration.session_id.clone()) }, at)?;
        Ok(vec![stopped, enabled])
    })?;
    enabled.map_err(|e| anyhow!(e))?;
    Ok(true)
}

/// Only a successful, structurally valid complete listing proves absence. An unavailable
/// daemon, unknown output format, or a failed command must never create another orchestrator.
async fn confirmed_absent(executable: &Path, session: &str, short: Option<&str>) -> bool {
    let output = tokio::time::timeout(Duration::from_secs(20), tokio::process::Command::new(executable)
        .args(["agents", "--json", "--all"]).kill_on_drop(true).stdin(std::process::Stdio::null()).output()).await;
    let Ok(Ok(output)) = output else { return false; };
    if !output.status.success() { return false; }
    let Ok(serde_json::Value::Array(agents)) = serde_json::from_slice(&output.stdout) else { return false; };
    agents.iter().all(|agent| agent.get("id").and_then(serde_json::Value::as_str).is_some_and(|id|
        !id.is_empty() && id != session && Some(id) != short && !session.starts_with(id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::{Event, HookEvent, config::Config, model::{Message, MessageFrom}};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn hub() -> Hub {
        let home = std::env::temp_dir().join(format!("alondra-migration-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        Hub::open(Config::resolve(home, &Default::default(), |_| None, || Some("Test".into())), false).unwrap()
    }
    fn append(hub: &Hub, e: Event) { hub.append_with(|_| Some(e)).unwrap(); }
    fn legacy(hub: &Hub) {
        append(hub, Event::Hook(HookEvent { session_id: "old-session".into(), event: "SessionStart".into(), ..Default::default() }));
        append(hub, Event::MarkedOrchestrator { session_id: "old-session".into() });
        append(hub, Event::Hook(HookEvent { session_id: "old-session".into(), event: "Stop".into(), ..Default::default() }));
    }
    #[cfg(unix)]
    fn fake(hub: &Hub, success: bool) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let file = hub.home.join("fake-claude");
        std::fs::write(&file, format!("#!/bin/sh\n[ \"$1\" = stop ] && [ \"$2\" = old-session ] || exit 9\nexit {}\n", if success { 0 } else { 1 })).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        file
    }
    #[tokio::test]
    async fn clean_install_enables_without_starting_claude() {
        let h = hub();
        assert!(prepare_with(&h, Path::new("/does/not/exist")).await.unwrap());
        assert!(h.read(|v| v.orchestrator.is_some() && v.orchestrator.as_ref().unwrap().session_id.is_none()));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn successful_stop_adopts_conversation_and_pending_messages_atomically() {
        let h = hub(); legacy(&h);
        append(&h, Event::MessageQueued(Message { id: 1, session_id: "old-session".into(), from: MessageFrom::User, text: "queued while idle".into(), answers_question: None, created_at: 1, handed_at: None, delivered_at: None, attachments: vec![], about: None }));
        assert!(prepare_with(&h, &fake(&h, true)).await.unwrap());
        h.read(|v| {
            assert_eq!(v.orchestrator.as_ref().unwrap().session_id.as_deref(), Some("old-session"));
            assert_eq!(v.messages[&1].session_id, orchestrator::NODE_ID);
            assert!(v.orchestrator_migration.is_none());
            assert!(v.removed.contains_key("old-session"));
            assert!(v.integrity_issues().is_empty());
        });
        let reopened = Hub::open(h.config.clone(), false).unwrap();
        assert!(prepare_with(&reopened, Path::new("/does/not/exist")).await.unwrap());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_failure_keeps_durable_reservation_and_never_enables_a_second_process() {
        let h = hub(); legacy(&h);
        assert!(prepare_with(&h, &fake(&h, false)).await.is_err());
        assert!(h.read(|v| v.orchestrator.is_none() && v.orchestrator_migration.is_some()));
        let reopened = Hub::open(h.config.clone(), false).unwrap();
        assert!(prepare_with(&reopened, &fake(&h, true)).await.unwrap());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn failed_stop_only_adopts_after_complete_inventory_proves_absence() {
        use std::os::unix::fs::PermissionsExt;
        for (listing, success) in [("[]", true), ("[{\"id\":\"old-session\"}]", false), ("[{\"id\":\"old-sess\"}]", false), ("{}", false), ("[{}]", false)] {
            let h = hub(); legacy(&h);
            let path = h.home.join("fake-inventory");
            std::fs::write(&path, format!("#!/bin/sh\nif [ \"$1\" = stop ]; then exit 1; fi\nprintf '%s' '{}'\n", listing)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(prepare_with(&h, &path).await.is_ok(), success, "{listing}");
        }
    }
    #[tokio::test]
    async fn busy_legacy_is_not_stopped_or_reserved() {
        let h = hub(); legacy(&h);
        append(&h, Event::Hook(HookEvent { session_id: "old-session".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        assert!(prepare_with(&h, Path::new("/does/not/exist")).await.unwrap_err().to_string().contains("busy"));
        assert!(h.read(|v| v.orchestrator.is_none() && v.orchestrator_migration.is_none()));
    }
}
