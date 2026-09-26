//! One durable inbox, one print-mode Claude invocation at a time. Worker nodes keep --bg.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use alondra_core::{now_millis, orchestrator::{self, Context, Outcome, Phase, RunKind, Transition}};
use anyhow::{Result, anyhow};
use crate::{hub::Hub, orchestrator_process::{self as process, Invocation, Ownership}, plugins};

const MAX_BATCH_MESSAGES: usize = 32;
const MAX_BATCH_BYTES: usize = 128 * 1024;

fn transition(hub: &Hub, transition: Transition) -> Result<()> {
    hub.append_all(|v| orchestrator::checked_transition(v, transition, now_millis()).map(|e| vec![e]))?
        .map_err(|e| anyhow!(e))?;
    Ok(())
}

pub async fn run(hub: Arc<Hub>, claude_home: PathBuf) {
    let directory = hub.home.join("runtime/orchestrator");
    // Keep ownership for this executor's lifetime. An inherited child descriptor also
    // fences daemon replacement if this process dies without finishing the durable run.
    let ownership = loop {
        match Ownership::acquire(&directory) {
            Ok(Some(lock)) => break lock,
            Ok(None) => {
                let run = hub.read(|v| v.orchestrator.as_ref().and_then(|r| r.active_run.as_ref().map(|r| r.id.clone())));
                let diagnostic = match process::terminate_orphan(&directory, run.as_deref()) {
                    Ok(()) => "Stopped an interrupted orchestrator process; waiting for its ownership lock to close".to_string(),
                    Err(e) => format!("Waiting for previous orchestrator ownership: {e:#}"),
                };
                let changed = hub.read(|v| v.orchestrator.as_ref().is_some_and(|r| r.last_error.as_deref() != Some(diagnostic.as_str())));
                if changed { let _ = transition(&hub, Transition::OwnershipWaiting { error: diagnostic }); }
            },
            Err(e) => log!("orchestrator ownership: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    };
    if let Err(e) = recover(&hub).await {
        log!("orchestrator recovery failed; executor stopped: {e}");
        return;
    }
    loop {
        let before = hub.changes();
        if let Err(e) = tick(&hub, &claude_home, &directory, &ownership).await {
            log!("orchestrator: {e}");
            // A persistence failure after spawning is ambiguous. Do not consume another batch.
            if let Err(e) = recover(&hub).await { log!("orchestrator recovery failed: {e}"); }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        hub.wait_changes(before, Duration::from_secs(5)).await;
    }
}

async fn recover(hub: &Hub) -> Result<()> {
    let _commands = hub.orchestrator_commands.write().await;
    let interrupted = hub.read(|v| v.orchestrator.as_ref().filter(|r| r.phase != Phase::Failed).and_then(|r| r.active_run.clone()));
    if let Some(run) = interrupted {
        transition(hub, Transition::Finished {
            run_id: run.id, outcome: Outcome::Ambiguous, context: None,
            error: Some("The daemon stopped during an orchestrator execution. Its effects must be checked before retrying.".into()),
            retry_at: None, compact_pending: false,
        })?;
    }
    Ok(())
}

async fn tick(hub: &Hub, claude_home: &Path, directory: &Path, ownership: &Ownership) -> Result<()> {
    if !crate::managed_orchestrator::prepare(hub).await? { return Ok(()); }
    execute_tick(hub, claude_home, directory, ownership, Path::new("claude")).await
}

async fn execute_tick(hub: &Hub, claude_home: &Path, directory: &Path, ownership: &Ownership, executable: &Path) -> Result<()> {
    let run_id = process::uuid()?;
    let claimed = hub.append_all(|v| {
        if !orchestrator::can_start(v, now_millis()) { return Err(()); }
        let runtime = v.orchestrator.as_ref().expect("can_start");
        let kind = if runtime.compact_pending { RunKind::Compact } else { RunKind::Work };
        let mut size = 0;
        let ids = if kind == RunKind::Work {
            orchestrator::pending_message_ids(v).into_iter().take(MAX_BATCH_MESSAGES).take_while(|id| {
                let n = v.messages.get(id).map_or(0, |m| m.text.len());
                let fits = size == 0 || size + n <= MAX_BATCH_BYTES;
                if fits { size += n; }
                fits
            }).collect()
        } else { vec![] };
        orchestrator::checked_transition(v, Transition::Started { run_id: run_id.clone(), kind, message_ids: ids }, now_millis()).map(|e| vec![e]).map_err(|_| ())
    })?;
    if claimed.is_err() { return Ok(()); }
    let runtime = hub.read(|v| v.orchestrator.clone()).expect("claimed runtime");
    let run = runtime.active_run.as_ref().expect("claimed execution");
    let resume = runtime.session_id.is_some();
    let session = match runtime.session_id.as_ref() { Some(id) => id.clone(), None => process::uuid()? };
    if !resume { transition(hub, Transition::SessionBound { run_id: run_id.clone(), session_id: session.clone() })?; }
    let found = plugins::dir(claude_home, plugins::NODE).zip(plugins::dir(claude_home, plugins::ORCHESTRATOR));
    let Some((node, orchestrator_plugin)) = found else {
        let _commands = hub.orchestrator_commands.write().await;
        return finish_failure(hub, &run_id, run.kind, false, "Required Alondra plugins are not installed; run alondra setup".into(), false);
    };
    let prompt = if run.kind == RunKind::Compact { "/compact".into() } else {
        let mut text = String::from("You are Alondra's orchestrator. Load the alondra-orchestrator skill. Process this durable inbox batch; use Alondra ask for user questions. After processing the batch, finish this invocation. Do not wait or poll for future work. The daemon will resume this conversation when there is more work.\n");
        if !runtime.pending_resolutions.is_empty() {
            text.push_str("\nDURABLE USER RESOLUTION: The user reviewed the interrupted executions below and chose to continue WITHOUT replaying their inbox batches. These resolutions override unfinished instructions in the older conversation. Do not repeat or continue those batches unless a NEW User message explicitly authorizes it. Automated Alondra updates are not user authorization. This does not mean their business actions succeeded or their underlying requests were cancelled. For unresolved work inspect fleet and ask the user instead of executing the previous intent.\n");
            for resolution in &runtime.pending_resolutions {
                text.push_str(&format!("Resolved execution {}: do not replay inbox message IDs {:?}.\n", resolution.run_id, resolution.message_ids));
            }
        }
        if !resume {
            text.push_str("This is a new conversation. Call fleet to inspect current operational state before acting. Do not infer that existing tasks need to be started again.\n");
        }
        hub.read(|v| { for id in &run.message_ids {
            if let Some(m) = v.messages.get(id) {
                text.push_str(&format!("\n--- Alondra inbox message {} ---\nSource: {:?}\n{}\n", id, m.from, m.text));
                if !m.attachments.is_empty() {
                    // Existing file metadata contains only Alondra-owned attachment references.
                    text.push_str(&format!("Attachments: {}\n", serde_json::to_string(&m.attachments).unwrap_or_default()));
                }
            }
        }});
        text
    };
    let output = process::execute(Invocation { executable, cwd: directory, home: &hub.home,
        plugins: [&node, &orchestrator_plugin], session: &session, resume, execution: &run_id, prompt: &prompt, ownership },
        || transition(hub, Transition::Running { run_id: run_id.clone() }), async {
            loop {
                let before = hub.changes();
                if hub.read(|v| v.orchestrator.as_ref().is_none_or(|r| r.active_run.as_ref().is_none_or(|run| run.id != run_id) || r.cancel_requested)) { break; }
                hub.wait_changes(before, Duration::from_secs(1)).await;
            }
        }).await;
    // Linearize completion after every accepted MCP operation (including its awaited side effects).
    let _commands = hub.orchestrator_commands.write().await;
    if hub.read(|v| v.orchestrator.as_ref().is_some_and(|r| r.cancel_requested)) {
        return transition(hub, Transition::Finished { run_id, outcome: Outcome::Ambiguous, context: None,
            error: Some("Orchestrator execution was cancelled; check its effects before retrying".into()), retry_at: None, compact_pending: runtime.compact_pending });
    }
    if resume && process::missing_session(&output, &session) {
        transition(hub, Transition::SessionMissing { run_id: run_id.clone() })?;
        if run.kind == RunKind::Compact {
            return transition(hub, Transition::Finished { run_id, outcome: Outcome::Success, context: None,
                error: Some("The saved conversation was missing during compaction; the next activation will create a new session".into()), retry_at: None, compact_pending: false });
        }
        return finish_failure(hub, &run_id, run.kind, false, "The saved Claude conversation is missing; a new conversation will be created".into(), false);
    }
    if !output.exit_ok || !output.stream.success || !output.stream.result || !output.stream.initialized || output.stream.error.is_some() {
        let error = output.stream.error.unwrap_or_else(|| if !output.diagnostic.trim().is_empty() { output.diagnostic } else { "Claude exited without a successful terminal result".into() });
        return finish_failure(hub, &run_id, run.kind, output.spawned, error, runtime.compact_pending);
    }
    let tokens = if run.kind == RunKind::Compact { output.stream.post_compact_tokens } else { output.stream.input_tokens };
    let context = tokens.map(|input_tokens| Context { input_tokens, window: output.stream.window.or_else(|| runtime.context.as_ref().and_then(|c| c.window)), model: output.stream.model.or_else(|| runtime.context.as_ref().and_then(|c| c.model.clone())), measured_at: now_millis() });
    let compact_pending = run.kind == RunKind::Work && hub.config.compact_orchestrator_at.is_some_and(|threshold| context.as_ref().is_some_and(|c| c.window.is_some_and(|window| window > 0 && c.input_tokens as f64 * 100.0 / window as f64 >= threshold)));
    // A successful /compact no-op is terminal, not a reason to retry forever.
    let notice = (run.kind == RunKind::Compact && !output.stream.compacted).then(|| "Claude completed /compact without a compact boundary; no context reduction was reported".into());
    transition(hub, Transition::Finished { run_id, outcome: Outcome::Success, context, error: notice, retry_at: None, compact_pending })
}

fn finish_failure(hub: &Hub, run_id: &str, kind: RunKind, spawned: bool, error: String, compact_pending: bool) -> Result<()> {
    let failures = hub.read(|v| v.orchestrator.as_ref().map_or(0, |r| r.consecutive_failures));
    let safe = (!spawned || kind == RunKind::Compact) && failures < 3;
    let outcome = if safe { Outcome::Retryable } else { Outcome::Ambiguous };
    let delay = 30_000_i64.saturating_mul(1_i64 << failures.min(5));
    transition(hub, Transition::Finished { run_id: run_id.into(), outcome, context: None, error: Some(error), retry_at: safe.then(|| now_millis().saturating_add(delay)), compact_pending })
}


#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::{Event, Message, MessageFrom};
    use std::os::unix::fs::PermissionsExt;

    fn fixture(body: &str) -> (Hub, PathBuf, PathBuf, PathBuf, Ownership) {
        let home = std::env::temp_dir().join(process::uuid().unwrap());
        let config = alondra_core::config::Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Test".into()));
        let hub = Hub::open(config, false).unwrap();
        transition(&hub, Transition::Enabled { session_id: None }).unwrap();
        let claude_home = home.join("claude-home");
        let repo = home.join("assets");
        for plugin in [plugins::NODE, plugins::ORCHESTRATOR] {
            std::fs::create_dir_all(repo.join(plugin).join(".claude-plugin")).unwrap();
            std::fs::write(repo.join(plugin).join(".claude-plugin/plugin.json"), "{}").unwrap();
        }
        std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
        std::fs::write(claude_home.join("plugins/known_marketplaces.json"), serde_json::json!({"alondra":{"installLocation":repo}}).to_string()).unwrap();
        let dir = home.join("runtime/orchestrator");
        let ownership = Ownership::acquire(&dir).unwrap().unwrap();
        let executable = home.join("fake-claude");
        std::fs::write(&executable, format!(r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
case "$1" in --session-id|--resume) shift; session="$1";; esac
shift
done
cat >/dev/null
printf 'start\n' >> starts
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"'"$session"'","permissionMode":"auto","model":"opus","mcp_servers":[{{"name":"alondra","status":"connected"}}],"tools":["mcp__alondra__fleet","mcp__alondra__ask"]}}'
{body}
"#)).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        (hub, claude_home, dir, executable, ownership)
    }
    fn enqueue(hub: &Hub, id: u64) {
        hub.append(Event::MessageQueued(Message { id, session_id: orchestrator::NODE_ID.into(), from: MessageFrom::User, text: format!("message {id}"), answers_question: None, created_at: now_millis(), handed_at: None, delivered_at: None, attachments: vec![], about: None })).unwrap();
    }
    const SUCCESS: &str = r#"printf '%s\n' '{"type":"assistant","message":{"model":"opus","usage":{"input_tokens":25,"cache_read_input_tokens":75}}}'
printf '%s\n' '{"type":"result","session_id":"'"$session"'","subtype":"success","is_error":false,"modelUsage":{"opus":{"contextWindow":1000000}}}'"#;

    #[tokio::test]
    async fn duplicate_wakeups_and_late_messages_do_not_share_a_claim() {
        let (hub, ch, dir, exe, lock) = fixture(&format!("sleep 0.2\n{SUCCESS}"));
        enqueue(&hub, 1);
        let first = execute_tick(&hub, &ch, &dir, &lock, &exe);
        let late = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !hub.read(|v| v.orchestrator.as_ref().is_some_and(|r| r.phase == Phase::Running)) { tokio::time::sleep(Duration::from_millis(5)).await; }
            }).await.unwrap();
            enqueue(&hub, 2);
            execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        };
        let (first, _) = tokio::join!(first, late);
        first.unwrap();
        hub.read(|v| {
            assert!(v.messages[&1].handed_at.is_some());
            assert!(v.messages[&2].handed_at.is_none());
            assert_eq!(v.orchestrator.as_ref().unwrap().context.as_ref().unwrap().input_tokens, 100);
        });
        assert_eq!(std::fs::read_to_string(dir.join("starts")).unwrap().lines().count(), 1);
        let session = hub.read(|v| v.orchestrator.as_ref().unwrap().session_id.clone());
        execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        assert_eq!(hub.read(|v| v.orchestrator.as_ref().unwrap().session_id.clone()), session);
        assert!(hub.read(|v| v.messages[&2].handed_at.is_some()));
        std::fs::remove_dir_all(&hub.home).unwrap();
    }
    #[tokio::test]
    async fn interrupted_execution_stays_claimed_across_restart_without_replay() {
        let (hub, ch, dir, exe, lock) = fixture("exit 1");
        enqueue(&hub, 1);
        execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        let restarted = Hub::open(hub.config.clone(), false).unwrap();
        recover(&restarted).await.unwrap();
        execute_tick(&restarted, &ch, &dir, &lock, &exe).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("starts")).unwrap().lines().count(), 1);
        restarted.read(|v| {
            let r = v.orchestrator.as_ref().unwrap();
            assert_eq!(r.phase, Phase::Failed);
            assert!(r.active_run.is_some());
            assert!(v.messages[&1].handed_at.is_none());
        });
        std::fs::remove_dir_all(&hub.home).unwrap();
    }
    #[tokio::test]
    async fn startup_before_result_commit_is_ambiguous_even_if_no_pid_remains() {
        let (hub, _, _, _, _) = fixture(SUCCESS);
        enqueue(&hub, 1);
        transition(&hub, Transition::Started { run_id: "interrupted".into(), kind: RunKind::Work, message_ids: vec![1] }).unwrap();
        let restarted = Hub::open(hub.config.clone(), false).unwrap();
        recover(&restarted).await.unwrap();
        assert!(restarted.read(|v| !orchestrator::can_start(v, now_millis())));
        std::fs::remove_dir_all(&hub.home).unwrap();
    }
    #[tokio::test]
    async fn explicit_missing_session_preserves_inbox_and_resets_only_identity() {
        let (hub, ch, dir, exe, lock) = fixture(SUCCESS);
        enqueue(&hub, 1);
        execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        enqueue(&hub, 2);
        std::fs::write(&exe, r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
case "$1" in --resume) shift; session="$1";; esac
shift
done
cat >/dev/null
printf '%s\n' "No conversation found with session ID: $session" >&2
printf '%s\n' '{"type":"result","session_id":"'"$session"'","subtype":"error_during_execution","is_error":true,"num_turns":0,"duration_api_ms":0,"errors":["No conversation found with session ID: '"$session"'"]}'
exit 1
"#).unwrap();
        execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        hub.read(|v| {
            let runtime = v.orchestrator.as_ref().unwrap();
            assert!(runtime.session_id.is_none());
            assert_eq!(runtime.generation, 2);
            assert!(runtime.active_run.is_none());
            assert!(runtime.retry_at.is_some());
            assert!(v.messages[&1].handed_at.is_some());
            assert!(v.messages[&2].handed_at.is_none());
        });
        std::fs::remove_dir_all(&hub.home).unwrap();
    }
    #[tokio::test]
    async fn compaction_keeps_late_work_pending_and_noop_does_not_loop() {
        let (mut hub, ch, dir, exe, lock) = fixture(&format!("sleep 0.2\n{SUCCESS}"));
        hub.config.compact_orchestrator_at = Some(0.001);
        enqueue(&hub, 1);
        execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        assert!(hub.read(|v| v.orchestrator.as_ref().unwrap().compact_pending));
        let compact = execute_tick(&hub, &ch, &dir, &lock, &exe);
        let late = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !hub.read(|v| v.orchestrator.as_ref().is_some_and(|r| r.phase == Phase::Compacting)) { tokio::time::sleep(Duration::from_millis(5)).await; }
            }).await.unwrap();
            enqueue(&hub, 2);
            execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        };
        let (result, _) = tokio::join!(compact, late);
        result.unwrap();
        hub.read(|v| {
            let r = v.orchestrator.as_ref().unwrap();
            assert_eq!(r.phase, Phase::Idle);
            assert!(!r.compact_pending);
            assert!(r.last_error.as_ref().unwrap().contains("without a compact boundary"));
            assert!(v.messages[&2].handed_at.is_none());
        });
        assert_eq!(std::fs::read_to_string(dir.join("starts")).unwrap().lines().count(), 2);
        std::fs::remove_dir_all(&hub.home).unwrap();
    }

    #[tokio::test]
    async fn durable_cancel_terminates_process_group_and_blocks_ambiguous_replay() {
        let (hub, ch, dir, exe, lock) = fixture("sleep 30 &\nwait");
        enqueue(&hub, 1);
        let work = execute_tick(&hub, &ch, &dir, &lock, &exe);
        let cancel = async {
            let run_id = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(id) = hub.read(|v| v.orchestrator.as_ref().filter(|r| r.phase == Phase::Running).and_then(|r| r.active_run.as_ref().map(|r| r.id.clone()))) { break id; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.unwrap();
            transition(&hub, Transition::CancelRequested { run_id }).unwrap();
        };
        let (result, _) = tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(work, cancel) }).await.unwrap();
        result.unwrap();
        hub.read(|v| {
            let runtime = v.orchestrator.as_ref().unwrap();
            assert!(runtime.last_error.as_deref().is_some_and(|e| e.contains("cancelled")));
            assert_eq!(runtime.phase, Phase::Failed);
            assert!(runtime.active_run.is_some());
            assert!(v.messages[&1].handed_at.is_none());
        });
        drop(lock);
        assert!(Ownership::acquire(&dir).unwrap().is_some(), "no descendant may retain execution ownership after cancellation");
        std::fs::remove_dir_all(&hub.home).unwrap();
    }

    #[tokio::test]
    async fn terminal_completion_waits_for_accepted_command_bookkeeping() {
        let (hub, ch, dir, exe, lock) = fixture(&format!("{SUCCESS}\ntouch process-done"));
        enqueue(&hub, 1);
        let accepted_command = hub.orchestrator_commands.read().await;
        let work = execute_tick(&hub, &ch, &dir, &lock, &exe);
        let handler = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !dir.join("process-done").exists() { tokio::time::sleep(Duration::from_millis(5)).await; }
            }).await.unwrap();
            assert_eq!(hub.read(|v| v.orchestrator.as_ref().unwrap().phase), Phase::Running);
            // Represents the durable bookkeeping after an already accepted external operation.
            enqueue(&hub, 2);
            drop(accepted_command);
        };
        let (result, _) = tokio::join!(work, handler);
        result.unwrap();
        assert!(hub.read(|v| v.messages[&1].handed_at.is_some() && v.messages[&2].handed_at.is_none()));
        std::fs::remove_dir_all(&hub.home).unwrap();
    }

    #[tokio::test]
    async fn missing_session_during_compaction_releases_maintenance_without_consuming_work() {
        let (mut hub, ch, dir, exe, lock) = fixture(SUCCESS);
        hub.config.compact_orchestrator_at = Some(0.001);
        enqueue(&hub, 1);
        execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        enqueue(&hub, 2);
        std::fs::write(&exe, r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
case "$1" in --resume) shift; session="$1";; esac
shift
done
cat >/dev/null
printf '%s\n' "No conversation found with session ID: $session" >&2
printf '%s\n' '{"type":"result","session_id":"'"$session"'","subtype":"error_during_execution","is_error":true,"num_turns":0,"duration_api_ms":0,"errors":["No conversation found with session ID: '"$session"'"]}'
exit 1
"#).unwrap();
        execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        hub.read(|v| {
            let r = v.orchestrator.as_ref().unwrap();
            assert_eq!(r.phase, Phase::Idle);
            assert!(r.session_id.is_none());
            assert!(r.active_run.is_none());
            assert!(!r.compact_pending);
            assert!(v.messages[&2].handed_at.is_none());
            assert_eq!(orchestrator::pending_message_ids(v), vec![2]);
        });
        std::fs::remove_dir_all(&hub.home).unwrap();
    }

    #[tokio::test]
    async fn acknowledged_interruption_injects_resolution_into_resumed_prompt() {
        let (hub, ch, dir, exe, lock) = fixture("exit 1");
        enqueue(&hub, 1);
        execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        let runtime = hub.read(|v| v.orchestrator.clone()).unwrap();
        transition(&hub, Transition::Acknowledged { run_id: runtime.active_run.unwrap().id }).unwrap();
        enqueue(&hub, 2);
        let script = std::fs::read_to_string(&exe).unwrap().replace("cat >/dev/null", "cat > received-prompt").replace("exit 1", SUCCESS);
        std::fs::write(&exe, script).unwrap();
        execute_tick(&hub, &ch, &dir, &lock, &exe).await.unwrap();
        let prompt = std::fs::read_to_string(dir.join("received-prompt")).unwrap();
        assert!(prompt.contains("WITHOUT replaying"));
        assert!(prompt.contains("do not replay inbox message IDs [1]"));
        assert!(prompt.contains("inbox message 2"));
        assert!(!prompt.contains("--- Alondra inbox message 1 ---"));
        hub.read(|v| {
            assert_eq!(v.orchestrator.as_ref().unwrap().session_id, runtime.session_id);
            assert!(v.orchestrator.as_ref().unwrap().pending_resolutions.is_empty());
        });
        std::fs::remove_dir_all(&hub.home).unwrap();
    }

}
