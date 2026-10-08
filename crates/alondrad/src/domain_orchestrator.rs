//! The orchestrator executor: one durable inbox batch per print-mode
//! Claude invocation of `mcp-orchestrator`, with a bearer bound to this
//! execution. The batch is claimed from the domain outbox before Claude runs
//! and finished as handed, retryable or uncertain afterwards; finishing an
//! invocation never completes a Task.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::OutboxMessage;
use crate::orchestrator_process::{self as process, Invocation, Ownership};
use alondra_core::delivery::State;
use alondra_core::domain::{DomainError, DomainState, InteractionId, InteractionPayload, QuestionOption, Scope};
use alondra_core::now_millis;
use alondra_core::orchestrator_batch::{BATCH_CONTINUE, BATCH_CONTINUE_LABEL, BATCH_PROBLEM_PREFIX, BATCH_RETRY, BATCH_RETRY_LABEL, BatchProblem, BatchStop};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const MAX_BATCH_MESSAGES: usize = 32;
const MAX_BATCH_BYTES: usize = 128 * 1024;
pub(crate) const FILE: &str = "orchestrator.json";
/// The runtime record's name before it was renamed; startup moves it to `FILE`.
pub(crate) const PREVIOUS_FILE: &str = "orchestrator-v2.json";
const SUMMARY_LINE: usize = 200;
const MAX_SUMMARY_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Runtime {
    pub session_id: Option<String>,
    pub active_run: Option<Run>,
    pub last_run_id: Option<String>,
    pub last_error: Option<String>,
    /// What kind of failure `last_error` describes, so the user reads it in their language.
    #[serde(default)]
    pub failure: Option<Failure>,
    pub retry_at: Option<i64>,
    pub consecutive_failures: u32,
    pub cancel_requested: bool,
    /// Batches whose execution was interrupted or given up: the next run is
    /// told never to replay them.
    pub pending_resolutions: Vec<Resolution>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Failure {
    /// Claude did not start, or ended without changing anything; the batch is offered again on its own.
    Launch,
    /// Claude ran but did not finish; some of its actions may have happened.
    Interrupted,
    /// One of Alondra's own tools was refused to it.
    Denied,
    /// The user stopped the run.
    Cancelled,
    /// Its saved conversation was gone; the next run starts a new one.
    SessionMissing,
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

/// A failed batch's Problem is `problem-orchestrator-<run id>`; its options are the decisions.
const PROBLEM_PREFIX: &str = BATCH_PROBLEM_PREFIX;
/// The answer to cards raised before "continue" and "stop" became one choice.
const PROBLEM_STOP: &str = "stop";
const SUBJECT_LINE: usize = 120;
/// A newer batch card replaces an open one.
const BATCH_CAUSE: &str = "orchestrator_batch";

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
            runtime.failure = None;
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
        runtime.failure = None;
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
        runtime.failure = None;
        runtime.retry_at = None;
        runtime.consecutive_failures = 0;
        save(&self.home, &runtime)
    }

    pub(crate) fn cancel(&self, run_id: &str) -> Result<()> {
        let mut runtime = load(&self.home)?;
        if runtime.active_run.as_ref().is_none_or(|run| run.id != run_id) {
            bail!("no running execution {run_id}");
        }
        if runtime.last_error.is_some() {
            return self.acknowledge(run_id);
        }
        runtime.cancel_requested = true;
        save(&self.home, &runtime)
    }

    fn prompt(&self, runtime: &Runtime, resume: bool, projects_root: Option<&Path>, messages: &[OutboxMessage]) -> Result<String> {
        let state = self.domain.state()?;
        let mut text = String::from("You are Alondra's orchestrator. Load the alondra-orchestrator skill. Process this inbox batch with the Alondra tools; use Alondra ask for user questions. After processing the batch, finish this invocation. Do not wait or poll for future work. The daemon will resume this conversation when there is more work.\n");
        if !runtime.pending_resolutions.is_empty() {
            text.push_str("\nDURABLE RESOLUTION: Previous interrupted inbox batches have already been resolved and are NOT replayed. Do not repeat or continue any of them unless a NEW User message explicitly authorizes it. This does not mean their business actions succeeded. An open Request below with no Task of its own was not turned into work: handle it now, unless fleet shows an open question about it.\n");
        }
        if !resume {
            text.push_str("This is a new conversation. Call fleet before acting; do not infer that existing tasks need to be started again.\n");
        }
        if let Some(root) = projects_root {
            text.push_str(&format!("Projects root, readable with Read, Glob and Grep: {}\n", root.display()));
        }
        let user = crate::domain_messages::user_name();
        text.push_str(&summary(&state, |request| crate::domain_orchestrator_tools::request_choices(&self.domain, request).ok().flatten()
            .map(|choices| crate::domain_orchestrator_tools::choices_text(&user, &self.home, &choices))));
        for message in messages {
            text.push_str(&format!("\n{}\n", crate::domain_transport::render(&state, message)));
        }
        Ok(text)
    }

    /// The domain names the orchestrator by this session; a failure never holds back the run.
    fn bind_session(&self, session: &str) {
        if let Err(error) = self.domain.bind_orchestrator_session(session) {
            log!("cannot record the orchestrator's session {session}: {error}");
        }
    }

    /// One batch: claim, bind a bearer to this execution, run Claude, finish.
    pub(crate) async fn tick(&self, ownership: &Ownership) -> Result<()> {
        self.apply_problem_answers()?;
        let mut runtime = load(&self.home)?;
        let now = now_millis();
        if runtime.active_run.is_some() || runtime.retry_at.is_some_and(|at| at > now) || self.domain.live.usage_pause.is_active() {
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
        if pending.is_empty() && runtime.pending_resolutions.is_empty() {
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
        if claimed.is_empty() && runtime.pending_resolutions.is_empty() {
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
        self.bind_session(&session);
        {
            let mut store = self.domain.store.lock().expect("domain store poisoned");
            for (attempt, _) in &run.attempts {
                if let Err(error) = store.record_orchestrator_delivery(*attempt, &session) {
                    log!("orchestrator delivery {attempt} to {session}: {error:#}");
                }
            }
        }
        let projects_root = crate::domain_schedule_runner::options_at(&self.home).ok()
            .map(|file| file.projects_root(&crate::options::user_home()))
            .filter(|root| root.is_dir());
        let prepared = self.domain.bind_orchestrator(&run_id)
            .map_err(|error| anyhow::anyhow!("cannot bind the execution's capability: {error}"))
            .and_then(|capability| Ok((capability, self.prompt(&runtime, resume, projects_root.as_deref(), &claimed)?)));
        let (capability, prompt) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.failed(runtime, &run, format!("{error:#}"), None),
        };
        let directory = self.home.join("runtime/orchestrator");
        let home = self.home.clone();
        let output = process::execute(Invocation {
            executable: &self.executable, cwd: &directory, home: &self.home,
            plugins: vec![node_plugin.as_path(), orchestrator_plugin.as_path()],
            domain: true, projects_root: projects_root.as_deref(),
            capability: Some(capability.expose_for_child_environment()),
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
        if let Err(error) = self.domain.revoke_execution(&run_id) {
            log!("orchestrator: cannot persist the revoked capability of {run_id}: {error:#}");
        }
        let mut runtime = load(&self.home)?;
        let Some(run) = runtime.active_run.clone() else { return Ok(()) };
        let succeeded = output.exit_ok && output.stream.success && output.stream.result
            && output.stream.initialized && output.stream.error.is_none();
        let effects = |stop: BatchStop| output.stream.may_have_effects().then(|| BatchProblem {
            stop, calls: output.stream.alondra_calls.clone(), subjects: self.subjects(&claimed), error: String::new(),
        });
        if runtime.cancel_requested {
            let Some(problem) = effects(BatchStop::Cancelled) else {
                self.give_up(&run, "orchestrator_cancelled")?;
                runtime.active_run = None;
                runtime.pending_resolutions.push(Resolution { run_id: run.id, message_ids: run.message_ids });
                runtime.last_error = None;
                runtime.failure = None;
                runtime.cancel_requested = false;
                return save(&self.home, &runtime);
            };
            self.finish(&run, State::Uncertain, Some("orchestrator_cancelled"))?;
            runtime.last_error = Some("Orchestrator execution was cancelled; check its effects before retrying".into());
            runtime.failure = Some(Failure::Cancelled);
            runtime.consecutive_failures = runtime.consecutive_failures.saturating_add(1);
            save(&self.home, &runtime)?;
            self.raise_batch_problem(&run.id, problem, runtime.last_error.as_deref().unwrap_or_default());
            return Ok(());
        }
        if resume && process::missing_session(&output, &session) {
            self.finish(&run, State::Retryable, Some("orchestrator_session_missing"))?;
            runtime.session_id = None;
            runtime.active_run = None;
            runtime.last_error = Some("The saved Claude conversation is missing; a new conversation will be created".into());
            runtime.failure = Some(Failure::SessionMissing);
            return save(&self.home, &runtime);
        }
        if !succeeded && let Some(until) = self.usage_limit(&output) {
            self.domain.live.usage_pause.exhaust(until, now_millis());
            if !output.stream.worked {
                self.finish(&run, State::Retryable, Some("orchestrator_usage_exhausted"))?;
                runtime.active_run = None;
                runtime.last_error = None;
                runtime.failure = None;
                runtime.retry_at = None;
                return save(&self.home, &runtime);
            }
        }
        if succeeded {
            self.finish(&run, State::Handed, None)?;
            runtime.active_run = None;
            runtime.last_error = None;
            runtime.failure = None;
            runtime.retry_at = None;
            runtime.consecutive_failures = 0;
            runtime.pending_resolutions.clear();
            return save(&self.home, &runtime);
        }
        let problem = effects(if output.stream.denied { BatchStop::Denied } else { BatchStop::Interrupted });
        let error = output.stream.error.unwrap_or_else(|| if !output.diagnostic.trim().is_empty() {
            output.diagnostic
        } else {
            "Claude exited without a successful terminal result".into()
        });
        self.failed(runtime, &run, error, problem)
    }

    /// What a batch was about, as the user knows it: the cards and tasks its messages came from.
    fn subjects(&self, messages: &[OutboxMessage]) -> Vec<String> {
        let Ok(state) = self.domain.state() else { return vec![] };
        let mut subjects: Vec<String> = vec![];
        for message in messages {
            let interaction = message.interaction_id.as_ref().and_then(|id| state.interactions.get(&InteractionId(id.clone())));
            let task_id = message.task_id.clone().or_else(|| match interaction.map(|interaction| &interaction.scope) {
                Some(Scope::Task { task_id }) => Some(task_id.0.clone()),
                _ => None,
            });
            let task = task_id.and_then(|id| state.tasks.values().find(|task| task.id.0 == id));
            let subject = match (interaction, task) {
                (Some(interaction), Some(task)) => format!("{}: {}", task.title, interaction.card.need),
                (Some(interaction), None) => interaction.card.need.clone(),
                (None, Some(task)) => task.title.clone(),
                (None, None) => message.message.lines().next().unwrap_or_default().chars().take(SUBJECT_LINE).collect(),
            };
            if !subject.trim().is_empty() && !subjects.contains(&subject) {
                subjects.push(subject);
            }
        }
        subjects
    }

    /// Whether the run failed because usage ran out, with the reset Claude reported if it did.
    fn usage_limit(&self, output: &process::Output) -> Option<Option<i64>> {
        let said = |text: &str| crate::usage_pause::says_usage_limit(text).then(|| crate::usage_pause::reset_in(text, now_millis()));
        if output.stream.limited {
            return Some(output.stream.limit_reset.or_else(|| self.domain.live.mods.reset_after_limit(now_millis())));
        }
        output.stream.error.as_deref().and_then(said).or_else(|| said(&output.diagnostic))
    }

    /// A run that changed nothing (`problem` is None) is offered again with a fresh prompt; one
    /// that may have changed something is the user's call.
    fn failed(&self, mut runtime: Runtime, run: &Run, error: String, problem: Option<BatchProblem>) -> Result<()> {
        runtime.consecutive_failures = runtime.consecutive_failures.saturating_add(1);
        runtime.last_error = Some(error);
        let Some(problem) = problem else {
            self.finish(run, State::Retryable, Some("orchestrator_launch_failed"))?;
            runtime.active_run = None;
            let delay = 30_000_i64.saturating_mul(1_i64 << runtime.consecutive_failures.min(5));
            runtime.retry_at = Some(now_millis().saturating_add(delay));
            runtime.failure = Some(Failure::Launch);
            return save(&self.home, &runtime);
        };
        self.finish(run, State::Uncertain, Some("orchestrator_execution_ambiguous"))?;
        runtime.failure = Some(if problem.stop == BatchStop::Denied { Failure::Denied } else { Failure::Interrupted });
        save(&self.home, &runtime)?;
        self.raise_batch_problem(&run.id, problem, runtime.last_error.as_deref().unwrap_or_default());
        Ok(())
    }

    /// A batch whose effects are unknown is the user's call: a Problem card with the ways out,
    /// which `apply_problem_answers` carries out.
    fn raise_batch_problem(&self, run_id: &str, mut problem: BatchProblem, error: &str) {
        let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None, inputs: vec![] };
        problem.error = error.to_owned();
        let (need, why) = problem.text(alondra_core::views::Lang::En);
        let raised = self.domain.raise_worded_problem(
            &format!("{PROBLEM_PREFIX}{run_id}"),
            BATCH_CAUSE,
            need,
            why,
            problem.to_body(),
            vec![option(BATCH_CONTINUE, BATCH_CONTINUE_LABEL), option(BATCH_RETRY, BATCH_RETRY_LABEL)],
            Some(BATCH_CONTINUE),
        );
        if let Err(error) = raised {
            log!("orchestrator: could not raise the failed batch's Problem: {error:?}");
        }
    }

    /// The user's answer to a failed batch's Problem is the decision on that execution; a
    /// Problem whose execution was settled some other way stops waiting.
    pub(crate) fn apply_problem_answers(&self) -> Result<()> {
        let state = self.domain.state()?;
        let active = load(&self.home)?.active_run.map(|run| run.id);
        for (id, interaction) in &state.interactions {
            let Some(run_id) = id.0.strip_prefix(PROBLEM_PREFIX) else { continue };
            let InteractionPayload::Question { answer, .. } = &interaction.payload else { continue };
            if interaction.withdrawn {
                continue;
            }
            match answer {
                Some(_) if active.as_deref() != Some(run_id) => {}
                Some(answer) => {
                    let applied = match answer.option_id.as_str() {
                        BATCH_RETRY => self.retry(run_id),
                        BATCH_CONTINUE => self.acknowledge(run_id),
                        PROBLEM_STOP => self.cancel(run_id),
                        other => Err(anyhow::anyhow!("unknown option {other}")),
                    };
                    if let Err(error) = applied {
                        log!("orchestrator: answer {} to {id} no longer applies: {error:#}", answer.option_id);
                    }
                }
                None if active.as_deref() != Some(run_id) => {
                    let _ = self.domain.withdraw_problem(id);
                }
                None => {}
            }
        }
        Ok(())
    }
}

/// The durable domain as the orchestrator needs it to act on a batch: its MCP has no read
/// tool, so every open Request and every Task not archived travels in the prompt.
fn summary(state: &DomainState, choices: impl Fn(&alondra_core::domain::RequestId) -> Option<String>) -> String {
    let mut text = String::from("\nDURABLE DOMAIN (open Requests and Tasks not archived):\n");
    let one_line = |body: &str| body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(SUMMARY_LINE).collect::<String>();
    for request in state.requests.values().filter(|request| !request.closed) {
        let unsplit = !state.tasks.values().any(|task| task.request_id == request.id);
        let body = if unsplit { request.body.trim().to_owned() } else { one_line(&request.body) };
        text.push_str(&format!("request {}: {body}\n", request.id));
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

/// The orchestrator phase and the actions available to the user.
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
        "queue_depth": queue_depth, "last_error": runtime.last_error, "failure": runtime.failure, "retry_at": runtime.retry_at,
    }))
}

pub(crate) fn executor(domain: Arc<DomainRuntime>, home: &Path) -> Executor {
    Executor { domain, home: home.to_owned(), claude_home: crate::watcher::claude_home(), executable: PathBuf::from("claude") }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision { Cancel, Acknowledge, Retry }

/// `POST /orchestrator/{cancel,acknowledge,retry}`, the user's decision on an execution.
pub(crate) fn decision_route(path: &str) -> Option<Decision> {
    match path {
        "/orchestrator/cancel" => Some(Decision::Cancel),
        "/orchestrator/acknowledge" => Some(Decision::Acknowledge),
        "/orchestrator/retry" => Some(Decision::Retry),
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
        return Err(error.context("orchestrator recovery failed"));
    }
    if let Some(session) = load(&home).ok().and_then(|runtime| runtime.session_id) {
        executor.bind_session(&session);
    }
    let mut changed = executor.domain.subscribe();
    let mut usage = executor.domain.live.usage_pause.subscribe();
    loop {
        if let Err(error) = executor.tick(&ownership).await {
            log!("orchestrator: {error:#}");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        tokio::select! {
            _ = changed.changed() => {},
            _ = usage.changed() => {},
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
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"'"$session"'","permissionMode":"auto","model":"opus","mcp_servers":[{{"name":"plugin:alondra-orchestrator:alondra-orchestrator","status":"connected"}}],"tools":["Skill","Read","mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task","mcp__plugin_alondra-orchestrator_alondra-orchestrator__ask"]}}'
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
    use alondra_core::domain::*;
    use std::collections::BTreeSet;

    const SUCCESS: &str = STUB_SUCCESS;

    /// Claude created a task and then died: the batch changed something.
    const CREATED_THEN_EXIT: &str = r#"printf '%s\n' '{"type":"assistant","session_id":"'"$session"'","message":{"model":"opus","content":[{"type":"tool_use","id":"toolu_create","name":"mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task","input":{"request_id":"r","title":"Push badge"}}],"usage":{"input_tokens":3}}}'
exit 1"#;

    /// Recorded from the 2026-10-08 runs 5e2de560, 6d5f82fa and c5d61e48: the prompt reached
    /// Claude too late and the resumed conversation had nothing to continue.
    const NO_DEFERRED_MARKER: &str = r#"printf '%s\n' 'Warning: no stdin data received in 3s, proceeding without it. If piping from a slow command, redirect stdin explicitly: < /dev/null to skip, or wait longer.' >&2
printf '%s\n' 'Error: No deferred tool marker found in the resumed session. Either the session was not deferred, the marker is stale (tool already ran), or it exceeds the tail-scan window. Provide a prompt to continue the conversation.' >&2
exit 1"#;

    struct Fixture { root: PathBuf, executor: Executor, ownership: Ownership }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
    }

    fn fixture(body: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("alondra-orchestrator-{}", crate::auth::random_hex(8)));
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
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// A question from an Agent to the orchestrator: one orchestrator inbox message.
    fn ask_orchestrator(domain: &DomainRuntime, n: u32) {
        if n == 1 {
            command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into() });
            command(domain, Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
            command(domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
                task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
                review_prs: false, dependencies: BTreeSet::new(), stack: None,
            });
        }
        command(domain, Actor::Agent { agent_id: "a".into() }, &format!("question-{n}"), Command::AskQuestion {
            attachments: vec![], interaction_id: format!("q{n}").into(), scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::Orchestrator, body: format!("Where do I put change {n}?"), blocking: false,
            options: vec![QuestionOption { id: "here".into(), label: "Here".into(), description: None, inputs: vec![] }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
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
        assert!(!prompt.contains("data_epoch=") && prompt.contains("Where do I put change 1?")
            && prompt.contains("Where do I put change 2?") && !prompt.contains("inbox message")
            && !prompt.contains("interaction_id=") && prompt.contains("new conversation"), "{prompt}");
        assert!(prompt.contains("request r: Work\n") && prompt.contains("task t of request r: \"Task\" state=not_started agent=a\n"), "{prompt}");
        assert!(outbox(domain).is_empty());
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "handed"));
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.session_id.is_some() && runtime.last_error.is_none());
        let session = runtime.session_id.clone().unwrap();
        assert_eq!(domain.state().unwrap().orchestrator_session, Some(session.as_str().into()), "the domain names the orchestrator by its session");
        {
            let store = domain.store.lock().unwrap();
            for item in store.delivery_evidence_page(0).unwrap().into_iter().filter(|i| i.recipient_kind == "orchestrator") {
                assert_eq!(store.orchestrator_delivery(item.attempt_id).unwrap().as_deref(), Some(session.as_str()), "each message records the session it was handed to");
            }
        }
        let token = std::fs::read_to_string(dir.join("capabilities")).unwrap().trim().to_owned();
        assert_eq!(token.len(), 64);
        assert!(domain.resolve_bearer(&token).is_err(), "the execution's bearer is revoked when it ends");
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("starts")).unwrap().lines().count(), 1, "nothing pending, no launch");
        ask_orchestrator(domain, 3);
        f.executor.tick(&f.ownership).await.unwrap();
        let prompt = std::fs::read_to_string(dir.join("received-prompt")).unwrap();
        assert!(!prompt.contains("new conversation") && prompt.contains("Where do I put change 3?")
            && !prompt.contains("Where do I put change 1?") && !prompt.contains("inbox message"), "{prompt}");
        assert_eq!(load(&f.executor.home).unwrap().session_id, runtime.session_id, "the conversation is resumed");
    }

    /// Recorded from the 2026-10-03 run e8ab8f90: Read on the projects root was refused, the
    /// orchestrator went on to create and launch its task, and Claude reported success.
    const REFUSED_READ_THEN_SUCCESS: &str = r#"printf '%s\n' '{"type":"assistant","session_id":"'"$session"'","message":{"model":"opus","content":[{"type":"tool_use","id":"toolu_01RqEHPzQNkeM7Upm8Mc7Yqu","name":"Read","input":{"file_path":"/Users/example/Projects"}}],"usage":{"input_tokens":2}}}'
printf '%s\n' '{"type":"user","session_id":"'"$session"'","message":{"role":"user","content":[{"type":"tool_result","content":"/Users/example/Projects is outside /Users/example/.alondra/runtime/orchestrator; --restricted confines the file tools to the working directory.","is_error":true,"tool_use_id":"toolu_01RqEHPzQNkeM7Upm8Mc7Yqu"}]}}'
printf '%s\n' '{"type":"assistant","session_id":"'"$session"'","message":{"model":"opus","content":[{"type":"tool_use","id":"toolu_create","name":"mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task","input":{"request_id":"r","title":"Push badge"}}],"usage":{"input_tokens":3}}}'
printf '%s\n' '{"type":"assistant","session_id":"'"$session"'","message":{"model":"opus","content":[{"type":"text","text":"Task created and launched."}],"usage":{"input_tokens":4}}}'
printf '%s\n' '{"type":"result","session_id":"'"$session"'","subtype":"success","is_error":false,"terminal_reason":"completed","num_turns":4,"result":"Task created and launched.","permission_denials":[{"tool_name":"Read","tool_use_id":"toolu_01RqEHPzQNkeM7Upm8Mc7Yqu","tool_input":{"file_path":"/Users/example/Projects"}}],"modelUsage":{"opus":{"contextWindow":1000000}}}'"#;

    #[tokio::test]
    async fn a_run_that_worked_around_a_refused_read_is_handed_and_frees_the_orchestrator() {
        let f = fixture(REFUSED_READ_THEN_SUCCESS);
        let domain = &f.executor.domain;
        let projects = f.root.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(alondra_core::options::path_in(&f.executor.home), format!("projects_root = {:?}\n", projects.to_str().unwrap())).unwrap();
        ask_orchestrator(domain, 1);
        f.executor.tick(&f.ownership).await.unwrap();
        let prompt = std::fs::read_to_string(f.executor.home.join("runtime/orchestrator/received-prompt")).unwrap();
        assert!(prompt.contains(&format!("Projects root, readable with Read, Glob and Grep: {}\n", projects.display())), "{prompt}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none(), "{runtime:?}");
        assert_eq!(runtime.last_error, None);
        assert_eq!(runtime.consecutive_failures, 0);
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "handed"));
        ask_orchestrator(domain, 2);
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(std::fs::read_to_string(f.executor.home.join("runtime/orchestrator/starts")).unwrap().lines().count(), 2,
            "the next batch runs at once");
    }

    #[tokio::test]
    async fn a_run_that_cannot_be_prepared_is_released_for_retry_instead_of_left_running() {
        let f = fixture(SUCCESS);
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        let blocked = f.executor.home.join("domain-capabilities.json.next");
        std::fs::create_dir_all(blocked.join("occupied")).unwrap();
        f.executor.tick(&f.ownership).await.unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none(), "{runtime:?}");
        assert!(runtime.last_error.as_deref().is_some_and(|error| error.contains("capability")), "{runtime:?}");
        assert!(runtime.retry_at.is_some());
        assert!(!f.executor.home.join("runtime/orchestrator/starts").exists());
        assert!(attempts(domain).iter().all(|(_, state, reason)| state == "retryable" && reason.as_deref() == Some("orchestrator_launch_failed")));
        assert_eq!(status(&f.executor).unwrap()["phase"], "failed");
        assert_eq!(status(&f.executor).unwrap()["failure"], "launch");

        std::fs::remove_dir_all(&blocked).unwrap();
        let mut runtime = load(&f.executor.home).unwrap();
        runtime.retry_at = None;
        save(&f.executor.home, &runtime).unwrap();
        f.executor.tick(&f.ownership).await.unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none(), "{runtime:?}");
        assert!(outbox(domain).is_empty());
    }

    #[tokio::test]
    async fn stale_orchestrator_messages_expire_before_execution_but_boundary_messages_remain() {
        let f = fixture(SUCCESS);
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        ask_orchestrator(domain, 2);
        command(domain, Actor::System, "user-info", Command::PublishInformation {
            interaction_id: "info-user".into(), scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::User, body: "User notification".into(), attachments: vec![], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
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

    fn answer_problem(domain: &DomainRuntime, run_id: &str, option: &str) {
        let state = domain.state().unwrap();
        domain.execute(&alondra_core::domain::Actor::User { user_id: "device:phone".into() }, &alondra_core::domain::CommandEnvelope {
            protocol_version: alondra_core::domain::PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("answer-{option}").into(),
            expected_revision: state.revision,
            command: alondra_core::domain::Command::AnswerQuestion {
                attachments: vec![], interaction_id: format!("{PROBLEM_PREFIX}{run_id}").into(), option_id: option.into(),
                inputs: Default::default(),
            },
        }, None, || Ok(())).unwrap();
    }

    #[tokio::test]
    async fn a_failed_batch_is_a_problem_card_and_its_answer_is_the_decision() {
        for (option, replays) in [("retry", true), ("continue", false)] {
            let f = fixture(CREATED_THEN_EXIT);
            let domain = &f.executor.domain;
            ask_orchestrator(domain, 1);
            f.executor.tick(&f.ownership).await.unwrap();
            let run = load(&f.executor.home).unwrap().active_run.expect("the failed batch stays claimed");
            let id = format!("{PROBLEM_PREFIX}{}", run.id);
            let problem = domain.state().unwrap().interactions[&id.clone().into()].clone();
            assert_eq!(problem.card.kind, alondra_core::domain::CardKind::Problem);
            assert_eq!(problem.author, alondra_core::domain::Author::System);
            let InteractionPayload::Question { options, .. } = &problem.payload else { panic!("a Problem is asked like a question") };
            assert_eq!(options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["continue", "retry"]);
            assert_eq!(problem.card.recommended.as_deref(), Some("continue"));
            let body = BatchProblem::from_body(&problem.body).expect("the body words the card in every language");
            assert_eq!(body.stop, BatchStop::Interrupted);
            assert_eq!(body.calls, ["create_task"]);
            assert_eq!(body.subjects, ["Task: Answer the question"]);
            assert!(body.error.contains("Claude exited"), "{body:?}");
            answer_problem(domain, &run.id, option);
            f.executor.apply_problem_answers().unwrap();
            let runtime = load(&f.executor.home).unwrap();
            assert!(runtime.active_run.is_none() && runtime.last_error.is_none(), "{option}: {runtime:?}");
            assert_eq!(!outbox(domain).is_empty(), replays, "{option}: only retry offers the batch again");
            assert_eq!(runtime.pending_resolutions.len(), usize::from(!replays), "{option}: the others are resolved without replay");
        }
    }

    #[tokio::test]
    async fn a_resumed_run_that_changed_nothing_runs_the_batch_again_with_its_prompt_and_asks_nobody() {
        let f = fixture(NO_DEFERRED_MARKER);
        let domain = &f.executor.domain;
        save(&f.executor.home, &Runtime { session_id: Some("saved".into()), ..Runtime::default() }).unwrap();
        ask_orchestrator(domain, 1);
        f.executor.tick(&f.ownership).await.unwrap();
        assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(PROBLEM_PREFIX)),
            "a run that changed nothing is not a decision for the user");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.retry_at.is_some(), "{runtime:?}");
        assert_eq!(runtime.failure, Some(Failure::Launch));
        assert!(runtime.last_error.as_deref().is_some_and(|error| error.contains("No deferred tool marker")), "{runtime:?}");
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "retryable"));

        let script = std::fs::read_to_string(&f.executor.executable).unwrap().replace(NO_DEFERRED_MARKER, SUCCESS);
        std::fs::write(&f.executor.executable, script).unwrap();
        let mut runtime = load(&f.executor.home).unwrap();
        runtime.retry_at = None;
        save(&f.executor.home, &runtime).unwrap();
        f.executor.tick(&f.ownership).await.unwrap();
        let prompt = std::fs::read_to_string(f.executor.home.join("runtime/orchestrator/received-prompt")).unwrap();
        assert!(prompt.contains("Where do I put change 1?") && !prompt.contains("new conversation"), "{prompt}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && runtime.session_id.as_deref() == Some("saved"), "{runtime:?}");
        assert!(outbox(domain).is_empty());
    }

    #[tokio::test]
    async fn a_run_stopped_before_it_changed_anything_is_settled_without_a_card() {
        let f = fixture("printf 'cancel\\n' > cancel-now\nexec sleep 30");
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        let home = f.executor.home.clone();
        let canceller = tokio::spawn(async move {
            let marker = home.join("runtime/orchestrator/cancel-now");
            while !marker.exists() { tokio::time::sleep(Duration::from_millis(20)).await; }
            let mut runtime = load(&home).unwrap();
            runtime.cancel_requested = true;
            save(&home, &runtime).unwrap();
        });
        f.executor.tick(&f.ownership).await.unwrap();
        canceller.await.unwrap();
        assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(PROBLEM_PREFIX)));
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && !runtime.cancel_requested, "{runtime:?}");
        assert_eq!(runtime.pending_resolutions.len(), 1, "the orchestrator hears it was stopped");
        assert!(outbox(domain).is_empty(), "a stopped batch is not replayed");
    }

    #[tokio::test]
    async fn a_batch_settled_elsewhere_withdraws_its_problem() {
        let f = fixture(CREATED_THEN_EXIT);
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        f.executor.tick(&f.ownership).await.unwrap();
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        f.executor.acknowledge(&run.id).unwrap();
        f.executor.apply_problem_answers().unwrap();
        assert!(domain.state().unwrap().interactions[&format!("{PROBLEM_PREFIX}{}", run.id).into()].withdrawn);
    }

    #[tokio::test]
    async fn a_failed_execution_is_ambiguous_until_acknowledged_and_resolved_at_restart() {
        let f = fixture(CREATED_THEN_EXIT);
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        f.executor.tick(&f.ownership).await.unwrap();
        let runtime = load(&f.executor.home).unwrap();
        let run = runtime.active_run.clone().expect("an ambiguous execution stays claimed");
        assert!(runtime.last_error.is_some());
        assert_eq!(runtime.failure, Some(Failure::Interrupted));
        assert!(attempts(domain).iter().all(|(_, state, reason)| state == "uncertain" && reason.as_deref() == Some("orchestrator_execution_ambiguous")));
        assert!(outbox(domain).is_empty(), "an ambiguous batch is not offered again");
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(std::fs::read_to_string(f.executor.home.join("runtime/orchestrator/starts")).unwrap().lines().count(), 1);
        assert!(f.executor.acknowledge("other-run").is_err());
        f.executor.acknowledge(&run.id).unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && runtime.failure.is_none());
        assert_eq!(runtime.pending_resolutions, vec![Resolution { run_id: run.id.clone(), message_ids: run.message_ids.clone() }]);
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "discarded"));

        let script = std::fs::read_to_string(&f.executor.executable).unwrap().replace("exit 1", SUCCESS);
        std::fs::write(&f.executor.executable, script).unwrap();
        ask_orchestrator(domain, 2);
        f.executor.tick(&f.ownership).await.unwrap();
        let prompt = std::fs::read_to_string(f.executor.home.join("runtime/orchestrator/received-prompt")).unwrap();
        assert!(prompt.contains("Previous interrupted inbox batches have already been resolved")
            && !prompt.contains(&run.id) && !prompt.contains("inbox message IDs"), "{prompt}");
        assert!(prompt.contains("Where do I put change 2?") && !prompt.contains("Where do I put change 1?")
            && !prompt.contains("inbox message"), "{prompt}");
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

    #[tokio::test]
    async fn an_interrupted_batch_is_not_replayed_but_one_fresh_run_sees_its_open_request() {
        let f = fixture(SUCCESS);
        let domain = &f.executor.domain;
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "bc2466f2".into(), body: "Make the editor readable".into(), auto_merge: false, maintenance: false, message: false,
        });
        let pending = outbox(domain)[0].0;
        let claim = domain.store.lock().unwrap().claim_message(pending).unwrap().unwrap();
        save(&f.executor.home, &Runtime {
            session_id: Some("session".into()),
            active_run: Some(Run { id: "interrupted".into(), message_ids: vec![claim.message.id],
                attempts: vec![(claim.attempt_id, claim.version)], started_at: 1, spawned: true }),
            ..Runtime::default()
        }).unwrap();
        f.executor.recover().unwrap();
        assert!(outbox(domain).is_empty(), "the interrupted batch is not replayed");

        f.executor.tick(&f.ownership).await.unwrap();
        let dir = f.executor.home.join("runtime/orchestrator");
        assert_eq!(std::fs::read_to_string(dir.join("starts")).unwrap().lines().count(), 1);
        let prompt = std::fs::read_to_string(dir.join("received-prompt")).unwrap();
        assert!(prompt.contains("Previous interrupted inbox batches have already been resolved")
            && prompt.contains("An open Request below with no Task of its own was not turned into work")
            && prompt.contains("request bc2466f2: Make the editor readable\n")
            && !prompt.contains("The user created Request"), "{prompt}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.pending_resolutions.is_empty() && runtime.active_run.is_none() && runtime.last_error.is_none());

        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("starts")).unwrap().lines().count(), 1, "one fresh run, not a loop");
    }

    #[test]
    fn each_open_request_carries_the_choices_it_was_asked_with() {
        let f = fixture(SUCCESS);
        ask_orchestrator(&f.executor.domain, 1);
        let state = f.executor.domain.state().unwrap();
        let text = summary(&state, |request| (request.0 == "r").then(|| "  options: project App · Effort: hard".to_owned()));
        assert!(text.contains("request r: Work\n  options: project App · Effort: hard\ntask t"), "{text}");
    }

    #[test]
    fn a_request_not_split_yet_reaches_the_orchestrator_whole() {
        let f = fixture(SUCCESS);
        let long = format!("{} end of the message", "word ".repeat(80));
        command(&f.executor.domain, Actor::User { user_id: "u".into() }, "long", Command::CreateRequest {
            request_id: "long".into(), body: long.clone(), auto_merge: false, maintenance: false, message: false,
        });
        let text = summary(&f.executor.domain.state().unwrap(), |_| None);
        assert!(text.contains("end of the message"), "{text}");
    }

    #[tokio::test]
    async fn the_user_decides_on_an_ambiguous_execution_through_its_status() {
        let f = fixture(CREATED_THEN_EXIT);
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
        assert_eq!(decision_route("/orchestrator/retry"), Some(Decision::Retry));
        assert_eq!(decision_route("/orchestrator/messages"), None);
    }

    #[tokio::test]
    async fn a_retry_offers_the_batch_again_and_a_cancel_is_ambiguous() {
        let f = fixture(CREATED_THEN_EXIT);
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

    fn starts(f: &Fixture) -> usize {
        std::fs::read_to_string(f.executor.home.join("runtime/orchestrator/starts")).map(|starts| starts.lines().count()).unwrap_or(0)
    }

    #[tokio::test]
    async fn no_batch_starts_while_usage_is_exhausted_and_it_runs_once_when_usage_is_back() {
        let f = fixture(SUCCESS);
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        domain.live.usage_pause.exhaust(Some(now_millis() + 3_600_000), now_millis());
        f.executor.tick(&f.ownership).await.unwrap();
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(starts(&f), 0, "paused: Claude is not started");
        assert_eq!(outbox(domain).len(), 1, "and the batch waits, untouched");
        assert!(load(&f.executor.home).unwrap().active_run.is_none());

        domain.live.usage_pause.resume(now_millis());
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(starts(&f), 1, "resumed: the pending batch runs");
        assert!(outbox(domain).is_empty());
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(starts(&f), 1, "and only once");
    }

    fn limited_once(reset_seconds: i64) -> String {
        format!(r#"if [ -f limited ]; then
{STUB_SUCCESS}
else
touch limited
printf '%s\n' '{{"type":"rate_limit_event","session_id":"'"$session"'","rate_limit_info":{{"status":"rejected","resetsAt":{reset_seconds},"rateLimitType":"five_hour"}}}}'
printf '%s\n' '{{"type":"result","session_id":"'"$session"'","subtype":"success","is_error":true,"result":"You have hit your session limit"}}'
exit 1
fi"#)
    }

    #[tokio::test]
    async fn a_run_that_hits_the_limit_before_working_pauses_everything_and_is_retried_after_the_reset() {
        let reset = now_millis() / 1000 + 3_600;
        let f = fixture(&limited_once(reset));
        let domain = &f.executor.domain;
        ask_orchestrator(domain, 1);
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(starts(&f), 1);
        assert_eq!(domain.live.usage_pause.current().map(|e| e.until), Some(reset * 1000), "until is the reset Claude reported");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && runtime.failure.is_none() && runtime.consecutive_failures == 0 && runtime.retry_at.is_none(), "{runtime:?}");
        assert_eq!(outbox(domain).iter().map(|(_, state)| state.as_str()).collect::<Vec<_>>(), ["retryable"]);
        assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(PROBLEM_PREFIX)), "a limit is not a Problem for the user to decide");

        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(starts(&f), 1, "nothing runs until the reset");

        domain.live.usage_pause.resume(now_millis());
        f.executor.tick(&f.ownership).await.unwrap();
        assert_eq!(starts(&f), 2, "the same batch is retried");
        assert!(outbox(domain).is_empty());
        assert!(attempts(domain).iter().any(|(_, state, _)| state == "handed"));
    }
}
