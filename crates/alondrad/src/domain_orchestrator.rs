//! The v2-only orchestrator executor: one durable inbox batch per print-mode
//! Claude invocation of `mcp-orchestrator`, with a bearer bound to this
//! execution. The batch is claimed from the domain outbox before Claude runs
//! and finished as handed, retryable or uncertain afterwards; finishing an
//! invocation never completes a Task.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::OutboxMessage;
use crate::orchestrator_process::{self as process, Invocation, Ownership};
use alondra_core::delivery::State;
use alondra_core::domain_v2::{DomainError, DomainState};
use alondra_core::now_millis;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const MAX_BATCH_MESSAGES: usize = 32;
const MAX_BATCH_BYTES: usize = 128 * 1024;
const FILE: &str = "orchestrator-v2.json";
const SUMMARY_LINE: usize = 200;
const MAX_SUMMARY_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Runtime {
    pub session_id: Option<String>,
    pub active_run: Option<Run>,
    pub last_run_id: Option<String>,
    pub last_error: Option<String>,
    pub retry_at: Option<i64>,
    pub consecutive_failures: u32,
    pub cancel_requested: bool,
    /// Batches whose execution was interrupted or given up: the next run is
    /// told never to replay them.
    pub pending_resolutions: Vec<Resolution>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Run {
    pub id: String,
    pub message_ids: Vec<i64>,
    pub attempts: Vec<(i64, u64)>,
    pub started_at: i64,
    pub spawned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Resolution {
    pub run_id: String,
    pub message_ids: Vec<i64>,
}

pub(crate) fn load(home: &Path) -> Result<Runtime> {
    match std::fs::read(home.join(FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid orchestrator runtime record"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Runtime::default()),
        Err(error) => Err(error.into()),
    }
}

fn save(home: &Path, runtime: &Runtime) -> Result<()> {
    let next = home.join(format!("{FILE}.next"));
    std::fs::write(&next, serde_json::to_vec(runtime)?)?;
    std::fs::rename(&next, home.join(FILE))?;
    Ok(())
}

pub(crate) struct Executor {
    pub domain: Arc<DomainRuntime>,
    pub home: PathBuf,
    pub claude_home: PathBuf,
    pub executable: PathBuf,
}

impl Executor {
    fn pending(&self) -> Result<Vec<OutboxMessage>> {
        let store = self.domain.store.lock().expect("domain store poisoned");
        Ok(store.pending_messages()?.into_iter()
            .filter(|message| message.recipient_kind == "orchestrator").collect())
    }

    /// An execution the daemon did not see finish is resolved at startup: its
    /// attempts stay uncertain evidence, the batch is never replayed, and the
    /// next run is told what was interrupted.
    pub(crate) fn recover(&self) -> Result<Runtime> {
        let mut runtime = load(&self.home)?;
        if let Some(run) = runtime.active_run.take() {
            self.give_up(&run, "daemon_restarted_during_execution")?;
            runtime.pending_resolutions.push(Resolution { run_id: run.id, message_ids: run.message_ids });
            runtime.last_error = None;
            runtime.retry_at = None;
            runtime.consecutive_failures = 0;
            runtime.cancel_requested = false;
            save(&self.home, &runtime)?;
        }
        Ok(runtime)
    }

    fn give_up(&self, run: &Run, reason: &str) -> Result<()> {
        let mut store = self.domain.store.lock().expect("domain store poisoned");
        for (attempt, version) in &run.attempts {
            store.finish_message(*attempt, *version, State::Uncertain, Some(reason))?;
            store.resolve_uncertain(*attempt, *version + 1, false)?;
        }
        Ok(())
    }

    fn finish(&self, run: &Run, state: State, reason: Option<&str>) -> Result<()> {
        let mut store = self.domain.store.lock().expect("domain store poisoned");
        for (attempt, version) in &run.attempts {
            store.finish_message(*attempt, *version, state, reason)?;
        }
        Ok(())
    }

    /// The user acknowledged an ambiguous execution: its batch is not replayed.
    pub(crate) fn acknowledge(&self, run_id: &str) -> Result<()> {
        let mut runtime = load(&self.home)?;
        let Some(run) = runtime.active_run.take().filter(|run| run.id == run_id) else {
            bail!("no ambiguous execution {run_id}");
        };
        let mut store = self.domain.store.lock().expect("domain store poisoned");
        for (attempt, version) in &run.attempts {
            store.resolve_uncertain(*attempt, *version + 1, false)?;
        }
        drop(store);
        runtime.pending_resolutions.push(Resolution { run_id: run.id, message_ids: run.message_ids });
        runtime.last_error = None;
        runtime.retry_at = None;
        runtime.consecutive_failures = 0;
        save(&self.home, &runtime)
    }

    /// The user chose to replay an ambiguous execution's batch.
    pub(crate) fn retry(&self, run_id: &str) -> Result<()> {
        let mut runtime = load(&self.home)?;
        let Some(run) = runtime.active_run.take().filter(|run| run.id == run_id) else {
            bail!("no ambiguous execution {run_id}");
        };
        let mut store = self.domain.store.lock().expect("domain store poisoned");
        for (attempt, version) in &run.attempts {
            store.resolve_uncertain(*attempt, *version + 1, true)?;
        }
        drop(store);
        runtime.last_error = None;
        runtime.retry_at = None;
        runtime.consecutive_failures = 0;
        save(&self.home, &runtime)
    }

    pub(crate) fn cancel(&self, run_id: &str) -> Result<()> {
        let mut runtime = load(&self.home)?;
        if runtime.active_run.as_ref().is_none_or(|run| run.id != run_id) {
            bail!("no running execution {run_id}");
        }
        runtime.cancel_requested = true;
        save(&self.home, &runtime)
    }

    fn prompt(&self, runtime: &Runtime, resume: bool, messages: &[OutboxMessage]) -> Result<String> {
        let state = self.domain.state()?;
        let mut text = String::from("You are Alondra's orchestrator. Load the alondra-orchestrator-domain-v2 skill. Process this durable inbox batch with typed domain commands; use Alondra ask for user questions. After processing the batch, finish this invocation. Do not wait or poll for future work. The daemon will resume this conversation when there is more work.\n");
        text.push_str(&format!("{}\n", crate::domain_transport::cursor(&state)));
        if !runtime.pending_resolutions.is_empty() {
            text.push_str("\nDURABLE RESOLUTION: the executions below were interrupted and their inbox batches are NOT replayed. Do not repeat or continue those batches unless a NEW User message explicitly authorizes it. This does not mean their business actions succeeded.\n");
            for resolution in &runtime.pending_resolutions {
                text.push_str(&format!("Resolved execution {}: do not replay inbox message IDs {:?}.\n", resolution.run_id, resolution.message_ids));
            }
        }
        if !resume {
            text.push_str("This is a new conversation. Read the durable domain before acting; do not infer that existing Tasks need to be started again.\n");
        }
        let user = crate::domain_messages::user_name();
        text.push_str(&summary(&state, |request| crate::domain_orchestrator_tools::request_choices(&self.domain, request).ok().flatten()
            .map(|choices| crate::domain_orchestrator_tools::choices_text(&user, &self.home, &choices))));
        for message in messages {
            text.push_str(&format!("\n--- Alondra inbox message {} ---\n{}\n", message.id, crate::domain_transport::render(&state, message)));
        }
        Ok(text)
    }

    /// One batch: claim, bind a bearer to this execution, run Claude, finish.
    pub(crate) async fn tick(&self, ownership: &Ownership) -> Result<()> {
        let mut runtime = load(&self.home)?;
        let now = now_millis();
        if runtime.active_run.is_some() || runtime.retry_at.is_some_and(|at| at > now) {
            return Ok(());
        }
        {
            let mut store = self.domain.store.lock().expect("domain store poisoned");
            store.discard_expired_orchestrator_messages(
                now.saturating_sub(crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS),
            )?;
        }
        let mut size = 0;
        let pending: Vec<OutboxMessage> = self.pending()?.into_iter().take(MAX_BATCH_MESSAGES)
            .take_while(|message| {
                let fits = size == 0 || size + message.message.len() <= MAX_BATCH_BYTES;
                if fits { size += message.message.len(); }
                fits
            }).collect();
        if pending.is_empty() {
            return Ok(());
        }
        let (Some(orchestrator_plugin), Some(node_plugin)) = (
            crate::plugins::dir(&self.claude_home, crate::plugins::ORCHESTRATOR),
            crate::plugins::dir(&self.claude_home, crate::plugins::NODE),
        ) else {
            bail!("Required Alondra plugins are not installed; run alondra setup");
        };
        let run_id = process::uuid()?;
        let resume = runtime.session_id.is_some();
        let session = match runtime.session_id.clone() {
            Some(session) => session,
            None => process::uuid()?,
        };
        let mut attempts = vec![];
        let mut claimed = vec![];
        {
            let mut store = self.domain.store.lock().expect("domain store poisoned");
            for message in pending {
                if let Some(claim) = store.claim_orchestrator_message(message.id)? {
                    attempts.push((claim.attempt_id, claim.version));
                    claimed.push(claim.message);
                }
            }
        }
        if claimed.is_empty() {
            return Ok(());
        }
        let run = Run {
            id: run_id.clone(), message_ids: claimed.iter().map(|m| m.id).collect(),
            attempts, started_at: now, spawned: false,
        };
        runtime.active_run = Some(run.clone());
        runtime.last_run_id = Some(run_id.clone());
        runtime.session_id = Some(session.clone());
        runtime.cancel_requested = false;
        save(&self.home, &runtime)?;
        let capability = self.domain.bind_orchestrator(&run_id)
            .map_err(|error| anyhow::anyhow!("cannot bind the execution's capability: {error}"))?;
        let prompt = self.prompt(&runtime, resume, &claimed)?;
        let directory = self.home.join("runtime/orchestrator");
        let home = self.home.clone();
        let output = process::execute(Invocation {
            executable: &self.executable, cwd: &directory, home: &self.home,
            plugins: vec![node_plugin.as_path(), orchestrator_plugin.as_path()],
            domain_v2: true, capability: Some(capability.expose_for_child_environment()),
            session: &session, resume, execution: &run_id, prompt: &prompt, ownership,
        }, |_| {
            let mut runtime = load(&home)?;
            if let Some(run) = runtime.active_run.as_mut() { run.spawned = true; }
            save(&home, &runtime)
        }, || Ok(()), async {
            loop {
                if load(&self.home).is_ok_and(|runtime| runtime.cancel_requested) { break; }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }).await;
        self.domain.revoke_execution(&run_id)?;
        let mut runtime = load(&self.home)?;
        let Some(run) = runtime.active_run.clone() else { return Ok(()) };
        let succeeded = output.exit_ok && output.stream.success && output.stream.result
            && output.stream.initialized && output.stream.error.is_none();
        if runtime.cancel_requested {
            self.finish(&run, State::Uncertain, Some("orchestrator_cancelled"))?;
            runtime.last_error = Some("Orchestrator execution was cancelled; check its effects before retrying".into());
            runtime.consecutive_failures = runtime.consecutive_failures.saturating_add(1);
            return save(&self.home, &runtime);
        }
        if resume && process::missing_session(&output, &session) {
            self.finish(&run, State::Retryable, Some("orchestrator_session_missing"))?;
            runtime.session_id = None;
            runtime.active_run = None;
            runtime.last_error = Some("The saved Claude conversation is missing; a new conversation will be created".into());
            return save(&self.home, &runtime);
        }
        if succeeded {
            self.finish(&run, State::Handed, None)?;
            runtime.active_run = None;
            runtime.last_error = None;
            runtime.retry_at = None;
            runtime.consecutive_failures = 0;
            runtime.pending_resolutions.clear();
            return save(&self.home, &runtime);
        }
        let error = output.stream.error.unwrap_or_else(|| if !output.diagnostic.trim().is_empty() {
            output.diagnostic
        } else {
            "Claude exited without a successful terminal result".into()
        });
        runtime.consecutive_failures = runtime.consecutive_failures.saturating_add(1);
        runtime.last_error = Some(error);
        // Before Claude ran, nothing happened and the batch may be offered
        // again; once spawned, its effects are unknown and the user decides.
        if !run.spawned && runtime.consecutive_failures < 3 {
            self.finish(&run, State::Retryable, Some("orchestrator_launch_failed"))?;
            runtime.active_run = None;
            let delay = 30_000_i64.saturating_mul(1_i64 << runtime.consecutive_failures.min(5));
            runtime.retry_at = Some(now_millis().saturating_add(delay));
        } else {
            self.finish(&run, State::Uncertain, Some("orchestrator_execution_ambiguous"))?;
        }
        save(&self.home, &runtime)
    }
}

/// The durable domain as the orchestrator needs it to act on a batch: its MCP has no read
/// tool, so every open Request and every Task not archived travels in the prompt.
fn summary(state: &DomainState, choices: impl Fn(&alondra_core::domain_v2::RequestId) -> Option<String>) -> String {
    let mut text = String::from("\nDURABLE DOMAIN (open Requests and Tasks not archived):\n");
    let one_line = |body: &str| body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(SUMMARY_LINE).collect::<String>();
    for request in state.requests.values().filter(|request| !request.closed) {
        text.push_str(&format!("request {}: {}\n", request.id, one_line(&request.body)));
        if let Some(chosen) = choices(&request.id) {
            text.push_str(&chosen);
            text.push('\n');
        }
    }
    for task in state.tasks.values().filter(|task| !task.archived) {
        let agent = task.agent_id.as_ref().map_or("none".to_owned(), |agent| agent.0.clone());
        let pr = task.pr_id.as_ref().map(|pr| format!(" pr={pr}")).unwrap_or_default();
        text.push_str(&format!("task {} of request {}: \"{}\" state={} agent={agent}{pr}\n",
            task.id, task.request_id, one_line(&task.title),
            serde_json::to_value(task.state).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()));
        if text.len() > MAX_SUMMARY_BYTES {
            text.push_str("(more Tasks not listed)\n");
            break;
        }
    }
    text
}

/// What `/v2/runtime` shows of the orchestrator: its phase and what the user may do about it.
pub(crate) fn status(executor: &Executor) -> Result<serde_json::Value> {
    let runtime = load(&executor.home)?;
    let queue_depth = executor.pending()?.len();
    let ambiguous = runtime.active_run.is_some() && runtime.last_error.is_some();
    let phase = match (&runtime.active_run, &runtime.last_error) {
        (Some(_), None) => "running",
        (_, Some(_)) => "failed",
        (None, None) => "idle",
    };
    Ok(serde_json::json!({
        "phase": phase, "can_retry": ambiguous, "can_cancel": runtime.active_run.is_some() && !ambiguous,
        "cancel_requested": runtime.cancel_requested,
        "active_run_id": runtime.active_run.as_ref().map(|run| run.id.clone()),
        "queue_depth": queue_depth, "last_error": runtime.last_error, "retry_at": runtime.retry_at,
    }))
}

pub(crate) fn executor(domain: Arc<DomainRuntime>, home: &Path) -> Executor {
    Executor { domain, home: home.to_owned(), claude_home: crate::watcher::claude_home(), executable: PathBuf::from("claude") }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision { Cancel, Acknowledge, Retry }

/// `POST /v2/orchestrator/{cancel,acknowledge,retry}`, the user's decision on an execution.
pub(crate) fn decision_route(path: &str) -> Option<Decision> {
    match path {
        "/v2/orchestrator/cancel" => Some(Decision::Cancel),
        "/v2/orchestrator/acknowledge" => Some(Decision::Acknowledge),
        "/v2/orchestrator/retry" => Some(Decision::Retry),
        _ => None,
    }
}

/// The decision as a local response: `{}` once applied, a conflict when no such execution waits.
pub(crate) fn decide(executor: &Executor, decision: Decision, body: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(run_id) = body.and_then(|body| body.get("run_id")).and_then(serde_json::Value::as_str) else {
        return serde_json::json!({"status": 422, "body": DomainError::new("invalid_input", "run_id is required")});
    };
    let applied = match decision {
        Decision::Cancel => executor.cancel(run_id),
        Decision::Acknowledge => executor.acknowledge(run_id),
        Decision::Retry => executor.retry(run_id),
    };
    match applied {
        Ok(()) => serde_json::json!({"status": 200, "body": {}}),
        Err(error) => serde_json::json!({"status": 409, "body": DomainError::new("conflict", error.to_string())}),
    }
}

pub(crate) async fn run(domain: Arc<DomainRuntime>, home: PathBuf, claude_home: PathBuf) -> Result<()> {
    let executor = Executor { domain, home: home.clone(), claude_home, executable: PathBuf::from("claude") };
    let directory = home.join("runtime/orchestrator");
    let ownership = loop {
        match Ownership::acquire(&directory) {
            Ok(Some(lock)) => break lock,
            Ok(None) => {
                let run = load(&home).ok().and_then(|runtime| runtime.active_run.map(|run| run.id));
                if let Err(error) = process::terminate_orphan(&directory, run.as_deref()) {
                    log!("waiting for previous orchestrator ownership: {error:#}");
                }
            }
            Err(error) => log!("orchestrator ownership: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    };
    if let Err(error) = executor.recover() {
        return Err(error.context("v2 orchestrator recovery failed"));
    }
    let mut changed = executor.domain.subscribe();
    loop {
        if let Err(error) = executor.tick(&ownership).await {
            log!("v2 orchestrator: {error:#}");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        tokio::select! {
            _ = changed.changed() => {},
            _ = tokio::time::sleep(Duration::from_secs(10)) => {},
        }
    }
}

/// A fake `claude` for the runner: it checks the v2 environment, records the prompt it was
/// handed and answers with `body`, and the plugins it needs are registered.
#[cfg(test)]
pub(crate) fn stub_claude(root: &Path, body: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let claude_home = root.join("claude-home");
    let repo = root.join("assets");
    for plugin in [crate::plugins::NODE, crate::plugins::ORCHESTRATOR] {
        std::fs::create_dir_all(repo.join(plugin).join(".claude-plugin")).unwrap();
        std::fs::write(repo.join(plugin).join(".claude-plugin/plugin.json"), "{}").unwrap();
    }
    std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
    std::fs::write(claude_home.join("plugins/known_marketplaces.json"),
        serde_json::json!({"alondra":{"installLocation":repo}}).to_string()).unwrap();
    let executable = root.join("fake-claude");
    std::fs::write(&executable, format!(r#"#!/bin/sh
test "$ALONDRA_DOMAIN_PROTOCOL" = 2 || exit 89
test -n "$ALONDRA_DOMAIN_CAPABILITY" || exit 88
printf '%s\n' "$ALONDRA_DOMAIN_CAPABILITY" >> capabilities
while [ "$#" -gt 0 ]; do
case "$1" in --session-id|--resume) shift; session="$1";; esac
shift
done
cat > received-prompt
printf 'start\n' >> starts
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"'"$session"'","permissionMode":"auto","model":"opus","mcp_servers":[{{"name":"plugin:alondra-orchestrator:alondra-orchestrator","status":"connected"}}],"tools":["Skill","Read","mcp__plugin_alondra-orchestrator_alondra-orchestrator__domain_command","mcp__plugin_alondra-orchestrator_alondra-orchestrator__ask"]}}'
{body}
"#)).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    (claude_home, executable)
}

#[cfg(test)]
pub(crate) const STUB_SUCCESS: &str = r#"printf '%s\n' '{"type":"assistant","message":{"model":"opus","usage":{"input_tokens":25}}}'
printf '%s\n' '{"type":"result","session_id":"'"$session"'","subtype":"success","is_error":false,"modelUsage":{"opus":{"contextWindow":1000000}}}'"#;

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::*;
    use std::collections::BTreeSet;

    const SUCCESS: &str = STUB_SUCCESS;

    struct Fixture { root: PathBuf, executor: Executor, ownership: Ownership }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
    }

    fn fixture(body: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("alondra-v2-orchestrator-{}", crate::auth::random_hex(8)));
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let (claude_home, executable) = stub_claude(&root, body);
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let directory = home.join("runtime/orchestrator");
        let ownership = Ownership::acquire(&directory).unwrap().unwrap();
        Fixture { root, executor: Executor { domain, home, claude_home, executable }, ownership }
    }

    fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch,
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// A question from an Agent to the orchestrator: one orchestrator inbox message.
    fn ask_orchestrator(domain: &DomainRuntime, n: u32) {
        if n == 1 {
            command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into() });
            command(domain, Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false });
            command(domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
                task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
                review_prs: false, dependencies: BTreeSet::new(), stack: None,
            });
        }
        command(domain, Actor::Agent { agent_id: "a".into() }, &format!("question-{n}"), Command::AskQuestion {
            attachments: vec![], interaction_id: format!("q{n}").into(), scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::Orchestrator, body: format!("Where do I put change {n}?"), blocking: false,
            options: vec![QuestionOption { id: "here".into(), label: "Here".into(), description: None, inputs: vec![] }],
        });
    }

    fn outbox(domain: &DomainRuntime) -> Vec<(i64, String)> {
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|m| m.recipient_kind == "orchestrator").map(|m| (m.id, m.state)).collect()
    }

    fn attempts(domain: &DomainRuntime) -> Vec<(i64, String, Option<String>)> {
        domain.store.lock().unwrap().delivery_evidence_page(0).unwrap().into_iter()
            .filter(|i| i.recipient_kind == "orchestrator").map(|i| (i.message_id, i.state, i.reason)).collect()
    }

    #[tokio::test]
    async fn a_batch_reaches_one_invocation_with_a_bound_capability_and_is_handed_once() {
        let f = fixture(SUCCESS);
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        ask_orchestrator(domain, 2);
        assert_eq!(outbox(domain).len(), 2);
        f.executor.tick(&f.ownership).await.unwrap();
        let dir = f.executor.home.join("runtime/orchestrator");
        assert_eq!(std::fs::read_to_string(dir.join("starts")).unwrap().lines().count(), 1);
        let prompt = std::fs::read_to_string(dir.join("received-prompt")).unwrap();
        assert!(prompt.contains("data_epoch=") && prompt.contains("inbox message 1") && prompt.contains("inbox message 2")
            && prompt.contains("interaction_id=q2") && prompt.contains("new conversation"), "{prompt}");
        assert!(prompt.contains("request r: Work\n") && prompt.contains("task t of request r: \"Task\" state=not_started agent=a\n"), "{prompt}");
        assert!(outbox(domain).is_empty());
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "handed"));
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.session_id.is_some() && runtime.last_error.is_none());
        let token = std::fs::read_to_string(dir.join("capabilities")).unwrap().trim().to_owned();
        assert_eq!(token.len(), 64);
        assert!(domain.resolve_bearer(&token).is_err(), "the execution's bearer is revoked when it ends");
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("starts")).unwrap().lines().count(), 1, "nothing pending, no launch");
        ask_orchestrator(domain, 3);
        f.executor.tick(&f.ownership).await.unwrap();
        let prompt = std::fs::read_to_string(dir.join("received-prompt")).unwrap();
        assert!(!prompt.contains("new conversation") && prompt.contains("inbox message 3") && !prompt.contains("inbox message 1"), "{prompt}");
        assert_eq!(load(&f.executor.home).unwrap().session_id, runtime.session_id, "the conversation is resumed");
    }

    #[tokio::test]
    async fn stale_orchestrator_messages_expire_before_execution_but_boundary_messages_remain() {
        let f = fixture(SUCCESS);
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        ask_orchestrator(domain, 2);
        command(domain, Actor::Agent { agent_id: "a".into() }, "user-info", Command::PublishInformation {
            interaction_id: "info-user".into(), scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::User, body: "User notification".into(), attachments: vec![],
        });
        let cutoff = now_millis().saturating_sub(crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS);
        {
            let mut store = domain.store.lock().unwrap();
            store.set_message_created_at_for_test(1, cutoff - 1).unwrap();
            store.set_message_created_at_for_test(2, cutoff).unwrap();
            let user_message = store.pending_messages().unwrap().into_iter()
                .find(|message| message.recipient_kind == "user").unwrap();
            store.set_message_created_at_for_test(user_message.id, cutoff - 1).unwrap();
            store.discard_expired_orchestrator_messages(cutoff).unwrap();
            assert_eq!(store.message_outbox_state_for_test(1).unwrap(), "discarded");
            assert_eq!(store.message_outbox_state_for_test(2).unwrap(), "pending");
            assert_eq!(store.message_outbox_state_for_test(user_message.id).unwrap(), "pending");
            store.set_message_created_at_for_test(
                2,
                now_millis().saturating_sub(crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS) + 60_000,
            ).unwrap();
        }

        f.executor.tick(&f.ownership).await.unwrap();

        let store = domain.store.lock().unwrap();
        assert_eq!(store.message_outbox_state_for_test(1).unwrap(), "discarded");
        assert_eq!(store.message_outbox_state_for_test(2).unwrap(), "handed");
        assert_eq!(store.pending_messages().unwrap().iter().filter(|message| message.recipient_kind == "user").count(), 1,
            "user notifications retain their separate delivery policy");
        assert!(store.pending_messages().unwrap().iter().all(|message| message.id != 1));
        drop(store);
        assert_eq!(std::fs::read_to_string(f.executor.home.join("runtime/orchestrator/starts")).unwrap().lines().count(), 1,
            "the fresh message remains eligible for execution");
    }

    #[test]
    fn orchestrator_claim_rechecks_expiry_and_keeps_the_exact_cutoff_eligible() {
        let f = fixture(SUCCESS);
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        ask_orchestrator(domain, 2);
        let created_at = 1_000;
        let mut store = domain.store.lock().unwrap();
        store.set_message_created_at_for_test(1, created_at).unwrap();
        store.set_message_created_at_for_test(2, created_at).unwrap();

        let exact_cutoff = store.claim_orchestrator_message_at_for_test(
            1,
            created_at + crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS,
        ).unwrap().expect("a message exactly 24 hours old remains eligible");
        assert_eq!(store.message_outbox_state_for_test(1).unwrap(), "dispatching");
        assert!(store.finish_message(exact_cutoff.attempt_id, exact_cutoff.version, State::Handed, None).unwrap());

        assert!(store.claim_orchestrator_message_at_for_test(
            2,
            created_at + crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS + 1,
        ).unwrap().is_none(), "a message older than 24 hours is not claimed");
        assert_eq!(store.message_outbox_state_for_test(2).unwrap(), "discarded");
        assert!(!store.delivery_evidence().unwrap().iter().any(|item| item.message_id == 2),
            "discarding an unclaimed message does not invent a delivery attempt");
    }

    #[tokio::test]
    async fn a_failed_execution_is_ambiguous_until_acknowledged_and_resolved_at_restart() {
        let f = fixture("exit 1");
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        f.executor.tick(&f.ownership).await.unwrap();
        let runtime = load(&f.executor.home).unwrap();
        let run = runtime.active_run.clone().expect("an ambiguous execution stays claimed");
        assert!(runtime.last_error.is_some());
        assert!(attempts(domain).iter().all(|(_, state, reason)| state == "uncertain" && reason.as_deref() == Some("orchestrator_execution_ambiguous")));
        assert!(outbox(domain).is_empty(), "an ambiguous batch is not offered again");
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(std::fs::read_to_string(f.executor.home.join("runtime/orchestrator/starts")).unwrap().lines().count(), 1);
        assert!(f.executor.acknowledge("other-run").is_err());
        f.executor.acknowledge(&run.id).unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none());
        assert_eq!(runtime.pending_resolutions, vec![Resolution { run_id: run.id.clone(), message_ids: run.message_ids.clone() }]);
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "discarded"));

        let script = std::fs::read_to_string(&f.executor.executable).unwrap().replace("exit 1", SUCCESS);
        std::fs::write(&f.executor.executable, script).unwrap();
        ask_orchestrator(domain, 2);
        f.executor.tick(&f.ownership).await.unwrap();
        let prompt = std::fs::read_to_string(f.executor.home.join("runtime/orchestrator/received-prompt")).unwrap();
        assert!(prompt.contains(&format!("Resolved execution {}: do not replay inbox message IDs {:?}", run.id, run.message_ids)), "{prompt}");
        assert!(prompt.contains("inbox message 2") && !prompt.contains("--- Alondra inbox message 1 ---"), "{prompt}");
        assert!(load(&f.executor.home).unwrap().pending_resolutions.is_empty());

        // A daemon restart during an execution resolves it without the user.
        let mut runtime = load(&f.executor.home).unwrap();
        ask_orchestrator(domain, 3);
        let pending = outbox(domain)[0].0;
        let claim = domain.store.lock().unwrap().claim_message(pending).unwrap().unwrap();
        runtime.active_run = Some(Run { id: "interrupted".into(), message_ids: vec![claim.message.id],
            attempts: vec![(claim.attempt_id, claim.version)], started_at: 1, spawned: true });
        save(&f.executor.home, &runtime).unwrap();
        let recovered = f.executor.recover().unwrap();
        assert!(recovered.active_run.is_none());
        assert_eq!(recovered.pending_resolutions[0].run_id, "interrupted");
        assert!(outbox(domain).is_empty(), "the interrupted batch is not replayed");
        assert!(attempts(domain).iter().any(|(id, state, _)| *id == claim.message.id && state == "discarded"));
    }

    #[test]
    fn each_open_request_carries_the_choices_it_was_asked_with() {
        let f = fixture(SUCCESS);
        ask_orchestrator(&f.executor.domain, 1);
        let state = f.executor.domain.state().unwrap();
        let text = summary(&state, |request| (request.0 == "r").then(|| "  options: project App · Effort: hard".to_owned()));
        assert!(text.contains("request r: Work\n  options: project App · Effort: hard\ntask t"), "{text}");
    }

    #[tokio::test]
    async fn the_user_decides_on_an_ambiguous_execution_through_its_status() {
        let f = fixture("exit 1");
        let domain = &f.executor.domain;
        assert_eq!(status(&f.executor).unwrap()["phase"], "idle");
        ask_orchestrator(domain, 1);
        assert_eq!(status(&f.executor).unwrap()["queue_depth"], 1);
        f.executor.tick(&f.ownership).await.unwrap();
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        let shown = status(&f.executor).unwrap();
        assert_eq!((shown["phase"].as_str(), shown["can_retry"].as_bool(), shown["can_cancel"].as_bool()), (Some("failed"), Some(true), Some(false)));
        assert_eq!(shown["active_run_id"], serde_json::json!(run.id));
        assert!(shown["last_error"].is_string());
        let missing = decide(&f.executor, Decision::Acknowledge, Some(&serde_json::json!({})));
        assert_eq!((missing["status"].as_u64(), missing["body"]["code"].as_str()), (Some(422), Some("invalid_input")));
        let other = decide(&f.executor, Decision::Acknowledge, Some(&serde_json::json!({"run_id": "other"})));
        assert_eq!((other["status"].as_u64(), other["body"]["code"].as_str()), (Some(409), Some("conflict")));
        let cancel = decide(&f.executor, Decision::Cancel, Some(&serde_json::json!({"run_id": "other"})));
        assert_eq!(cancel["status"], 409);
        let acknowledged = decide(&f.executor, Decision::Acknowledge, Some(&serde_json::json!({"run_id": run.id})));
        assert_eq!(acknowledged, serde_json::json!({"status": 200, "body": {}}));
        assert_eq!(status(&f.executor).unwrap()["phase"], "idle");
        assert_eq!(decision_route("/v2/orchestrator/retry"), Some(Decision::Retry));
        assert_eq!(decision_route("/v2/orchestrator/messages"), None);
    }

    #[tokio::test]
    async fn a_retry_offers_the_batch_again_and_a_cancel_is_ambiguous() {
        let f = fixture("exit 1");
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        f.executor.tick(&f.ownership).await.unwrap();
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        f.executor.retry(&run.id).unwrap();
        assert_eq!(outbox(domain).len(), 1);
        assert_eq!(outbox(domain)[0].1, "retryable");
        assert!(load(&f.executor.home).unwrap().active_run.is_none());
        assert!(f.executor.cancel("nothing").is_err(), "cancel needs a running execution");
    }
}
