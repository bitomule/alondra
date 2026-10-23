//! The orchestrator executor: one durable inbox batch per print-mode
//! Claude invocation of `mcp-orchestrator`, with a bearer bound to this
//! execution. The batch is claimed from the domain outbox before Claude runs
//! and finished as handed, retryable or uncertain afterwards; finishing an
//! invocation never completes a Task.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::OutboxMessage;
use crate::orchestrator_process::{self as process, BatchEnd, Ownership};
use alondra_core::delivery::State;
use alondra_core::domain::{Actor, Author, Command, CommandEnvelope, DomainError, DomainState, InteractionId, InteractionPayload, PROTOCOL_VERSION, QuestionOption, Recipient, RequestId, Scope};
use alondra_core::now_millis;
use alondra_core::orchestrator_batch::{BATCH_CONTINUE, BATCH_CONTINUE_LABEL, BATCH_PROBLEM_PREFIX, BATCH_RETRY, BATCH_RETRY_LABEL, BatchProblem, BatchStop};
use alondra_core::orchestrator_cards::{STUCK_AFTER, STUCK_PROBLEM_PREFIX, STUCK_RETRY, STUCK_RETRY_LABEL, StuckProblem};
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
/// The most of its final words a batch hands to the next one.
const MAX_NOTE_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Runtime {
    /// The conversation of the latest batch. Every batch starts its own: none is resumed.
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
    /// The Problem that tells the user Claude does not start, while it is open.
    #[serde(default)]
    pub stuck_problem: Option<String>,
    /// What the last handled batch said at its end, for the next batch, which starts a new
    /// conversation and has only what Alondra hands it.
    #[serde(default)]
    pub note: Option<String>,
    /// The context the latest batch read.
    #[serde(default)]
    pub last_context: Option<BatchContext>,
}

/// Tokens the model read in a batch's first call and in its last one: its context at the start
/// of the turn and at its end.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BatchContext {
    pub first: u64,
    pub last: u64,
    pub window: Option<u64>,
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
    /// Its saved conversation was gone; the next run starts a new one. Only a daemon that
    /// resumed conversations wrote it.
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

pub(crate) fn save(home: &Path, runtime: &Runtime) -> Result<()> {
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
const STUCK_CAUSE: &str = "orchestrator_stuck";

pub(crate) struct Executor {
    pub domain: Arc<DomainRuntime>,
    pub home: PathBuf,
    pub claude_home: PathBuf,
}

/// A notice for the orchestrator is read once Alondra handed it over (or gave it up): it no
/// longer waits for anyone.
pub(crate) fn read_handed_notices(domain: &DomainRuntime) {
    read_handed(domain, |_| {});
}

/// The startup sweep finds every notice an older daemon never read: 443 on the real data, each a
/// command that holds the domain store. Back to back they kept the store from the HTTP server's
/// two workers for seconds, so the sweep hands it back after each notice for as long as it held it.
pub(crate) fn read_handed_notices_in_background(domain: &DomainRuntime) {
    read_handed(domain, |held| std::thread::sleep(held.max(SWEEP_PAUSE)));
}

const SWEEP_PAUSE: Duration = Duration::from_millis(2);

fn read_handed(domain: &DomainRuntime, pause: impl Fn(Duration)) {
    let out = match domain.store.lock().expect("domain store poisoned").orchestrator_notices_out() {
        Ok(out) => out,
        Err(error) => return log!("could not list the orchestrator's handed notices: {error:#}"),
    };
    for id in out {
        let Ok(state) = domain.state() else { return };
        let unread = state.interactions.get(&InteractionId::from(id.as_str())).is_some_and(|i| {
            i.recipient == Recipient::Orchestrator && matches!(&i.payload, InteractionPayload::Information { read_by } if read_by.is_empty())
        });
        if !unread {
            continue;
        }
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("orchestrator-read-{id}").into(), expected_revision: state.revision,
            command: Command::MarkRead { interaction_id: id.as_str().into() },
        };
        let started = std::time::Instant::now();
        if let Err(error) = domain.execute(&Actor::System, &envelope, None, || Ok(())) {
            log!("could not mark the orchestrator's notice {id} read: {error:?}");
        }
        pause(started.elapsed());
    }
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
            if let Err(error) = process::end_recorded(&self.home.join("runtime/orchestrator"), &run.id) {
                log!("orchestrator: cannot end the process the previous daemon left running: {error:#}");
            }
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

    fn prompt(&self, runtime: &Runtime, projects_root: Option<&Path>, messages: &[OutboxMessage]) -> Result<String> {
        let state = self.domain.state()?;
        let mut text = String::from("You are Alondra's orchestrator. Load the alondra-orchestrator skill. Process this inbox batch with the Alondra tools; use Alondra ask for user questions. Every open Request below with no Task of its own leaves this batch with its Tasks, an answer given with close_request, or a question to the user: an answer written only in this conversation reaches nobody. After processing the batch, finish this invocation. Do not wait or poll for future work.\n");
        text.push_str("This is a new conversation, as every batch is: nothing of an earlier batch is in it but what Alondra writes below, which is current. Call fleet or node only for what it does not say; do not infer that existing tasks need to be started again. End with a short note, a few sentences at most, of what the next batch needs that Alondra does not already hold: it reaches the next batch and nobody else.\n");
        if let Some(note) = &runtime.note {
            text.push_str(&format!("\nYOUR NOTE FROM THE PREVIOUS BATCH:\n{note}\n"));
        }
        if !runtime.pending_resolutions.is_empty() {
            text.push_str("\nDURABLE RESOLUTION: Previous interrupted inbox batches have already been resolved and are NOT replayed. Do not repeat or continue any of them unless a NEW User message explicitly authorizes it. This does not mean their business actions succeeded. An open Request below with no Task of its own was not turned into work: handle it now, unless fleet shows an open question about it.\n");
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
    pub(crate) async fn tick(&self) -> Result<()> {
        self.apply_problem_answers()?;
        let mut runtime = load(&self.home)?;
        let now = now_millis();
        if runtime.active_run.is_some() || runtime.retry_at.is_some_and(|at| at > now) || self.domain.live.usage_pause.is_active() {
            return Ok(());
        }
        let Some(account) = crate::accounts::choose_for_launch(&self.domain, &self.home, now) else { return Ok(()) };
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
            if runtime.last_error.is_none() {
                crate::orchestrator_inbox::settle(&self.domain);
            }
            return Ok(());
        }
        let (Some(orchestrator_plugin), Some(node_plugin)) = (
            crate::plugins::dir(&self.claude_home, crate::plugins::ORCHESTRATOR),
            crate::plugins::dir(&self.claude_home, crate::plugins::NODE),
        ) else {
            bail!("Required Alondra plugins are not installed; run alondra doctor --fix");
        };
        let run_id = process::uuid()?;
        let session = process::uuid()?;
        let state = self.domain.state()?;
        let epoch = state.data_epoch.clone();
        let asked = crate::orchestrator_inbox::asked(&state);
        drop(state);
        let ids: Vec<i64> = pending.iter().map(|message| message.id).collect();
        let (claims, owner) = self.domain.store.lock().expect("domain store poisoned")
            .claim_orchestrator_batch(&ids, &run_id, &session, epoch)?;
        let attempts: Vec<(i64, u64)> = claims.iter().map(|claim| (claim.attempt_id, claim.version)).collect();
        let claimed: Vec<OutboxMessage> = claims.into_iter().map(|claim| claim.message).collect();
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
        let projects_root = crate::domain_schedule_runner::options_at(&self.home).ok()
            .map(|file| file.projects_root(&crate::options::user_home()))
            .filter(|root| root.is_dir());
        let prepared = self.domain.bind_orchestrator(&run_id)
            .map_err(|error| anyhow::anyhow!("cannot bind the execution's capability: {error}"))
            .and_then(|capability| Ok((capability, self.prompt(&runtime, projects_root.as_deref(), &claimed)?)));
        let (capability, prompt) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.failed(runtime, &run, format!("{error:#}"), None),
        };
        let directory = self.home.join("runtime/orchestrator");
        let home = self.home.clone();
        let mark_spawned = move || {
            let mut runtime = load(&home)?;
            if let Some(run) = runtime.active_run.as_mut() { run.spawned = true; }
            save(&home, &runtime)
        };
        let cancel = async {
            loop {
                if load(&self.home).is_ok_and(|runtime| runtime.cancel_requested) { break; }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        };
        let output = match self.domain.sdk() {
            Some((backend, _)) => crate::orchestrator_sdk::execute(crate::orchestrator_sdk::Batch {
                owner, backend: backend.clone(), home: self.home.clone(), directory: directory.clone(),
                node_plugin, orchestrator_plugin, projects_root: projects_root.clone(),
                capability: capability.expose_for_child_environment().to_owned(),
                session: session.clone(), prompt, account: account.clone(),
            }, mark_spawned, cancel).await,
            None => {
                drop(owner);
                process::Output::not_started(crate::sdk_owner::NO_HOST)
            }
        };
        let changes = self.domain.end_execution(&run_id);
        let mut runtime = load(&self.home)?;
        let Some(run) = runtime.active_run.clone() else { return Ok(()) };
        let end = if runtime.cancel_requested {
            BatchEnd::Cancelled { effects: (!changes.is_empty()).then_some(changes) }
        } else {
            output.end(&session, false)
        };
        if let Some(context) = output.stream.context() {
            log!("orchestrator: batch {} read {} tokens in its first call and {} in its last (window {})",
                run.id, context.first, context.last, context.window.map_or("unknown".to_owned(), |window| window.to_string()));
            runtime.last_context = Some(context);
        }
        if !matches!(end, BatchEnd::Handled | BatchEnd::Cancelled { .. } | BatchEnd::SessionMissing)
            && let Some(reset) = output.stream.limit(&output.diagnostic)
        {
            crate::usage_probe::record_limit(&self.domain, &account, reset, now_millis());
        }
        let problem = |stop: BatchStop, calls: Vec<String>| BatchProblem { stop, calls, subjects: self.subjects(&claimed), error: String::new() };
        match end {
            BatchEnd::Cancelled { effects: None } => {
                self.give_up(&run, "orchestrator_cancelled")?;
                runtime.active_run = None;
                runtime.pending_resolutions.push(Resolution { run_id: run.id, message_ids: run.message_ids });
                runtime.last_error = None;
                runtime.failure = None;
                runtime.cancel_requested = false;
                save(&self.home, &runtime)
            }
            BatchEnd::Cancelled { effects: Some(calls) } => {
                self.finish(&run, State::Uncertain, Some("orchestrator_cancelled"))?;
                runtime.last_error = Some("Orchestrator execution was cancelled; check its effects before retrying".into());
                runtime.failure = Some(Failure::Cancelled);
                runtime.consecutive_failures = runtime.consecutive_failures.saturating_add(1);
                save(&self.home, &runtime)?;
                self.raise_batch_problem(&run.id, problem(BatchStop::Cancelled, calls), runtime.last_error.as_deref().unwrap_or_default());
                Ok(())
            }
            BatchEnd::SessionMissing => {
                self.finish(&run, State::Retryable, Some("orchestrator_session_missing"))?;
                runtime.session_id = None;
                runtime.active_run = None;
                runtime.last_error = Some("The saved Claude conversation is missing; a new conversation will be created".into());
                runtime.failure = Some(Failure::SessionMissing);
                save(&self.home, &runtime)
            }
            BatchEnd::Limited { .. } => {
                self.finish(&run, State::Retryable, Some("orchestrator_usage_exhausted"))?;
                runtime.active_run = None;
                runtime.last_error = None;
                runtime.failure = None;
                runtime.retry_at = None;
                save(&self.home, &runtime)
            }
            BatchEnd::Handled => {
                runtime.note = output.stream.note.as_deref().map(|note| bounded_note(note, MAX_NOTE_BYTES)).filter(|note| !note.is_empty());
                self.handled(runtime, &run, &asked)
            }
            BatchEnd::NotStarted { why } => self.failed(runtime, &run, why, None),
            BatchEnd::Interrupted { calls, why } => self.failed(runtime, &run, why, Some(problem(BatchStop::Interrupted, calls))),
            BatchEnd::Denied { calls, why } => self.failed(runtime, &run, why, Some(problem(BatchStop::Denied, calls))),
        }
    }

    /// The batch's turn ended with its result: its messages were handed over. When nothing else
    /// waits for the orchestrator, whatever it left without an outcome is settled now, in this
    /// step; a message still waiting is the next batch's, and settling before it would close or
    /// remind about a request the orchestrator has not seen yet.
    fn handled(&self, mut runtime: Runtime, run: &Run, (requests, suggestions): &(Vec<RequestId>, Vec<InteractionId>)) -> Result<()> {
        self.finish(run, State::Handed, None)?;
        read_handed_notices(&self.domain);
        runtime.active_run = None;
        runtime.last_error = None;
        runtime.failure = None;
        runtime.retry_at = None;
        runtime.consecutive_failures = 0;
        runtime.pending_resolutions.clear();
        if let Some(problem) = runtime.stuck_problem.take() {
            let _ = self.domain.withdraw_problem(&InteractionId(problem));
        }
        save(&self.home, &runtime)?;
        if !self.pending()?.is_empty() {
            return Ok(());
        }
        crate::orchestrator_inbox::settle(&self.domain);
        let state = self.domain.state()?;
        let left = crate::orchestrator_inbox::unresolved(&state, requests, suggestions);
        if !left.is_empty() {
            log!("orchestrator: batch {} left without an outcome: {}", run.id, left.join(", "));
        }
        Ok(())
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
            if runtime.consecutive_failures >= STUCK_AFTER && runtime.stuck_problem.is_none() {
                runtime.stuck_problem = self.raise_stuck_problem(&run.id, runtime.consecutive_failures, runtime.last_error.as_deref().unwrap_or_default());
            }
            return save(&self.home, &runtime);
        };
        self.finish(run, State::Uncertain, Some("orchestrator_execution_ambiguous"))?;
        runtime.failure = Some(if problem.stop == BatchStop::Denied { Failure::Denied } else { Failure::Interrupted });
        save(&self.home, &runtime)?;
        self.raise_batch_problem(&run.id, problem, runtime.last_error.as_deref().unwrap_or_default());
        Ok(())
    }

    /// Claude did not start several times in a row: the user hears of it, with a way to try at once.
    fn raise_stuck_problem(&self, run_id: &str, failures: u32, error: &str) -> Option<String> {
        let id = format!("{STUCK_PROBLEM_PREFIX}{run_id}");
        let problem = StuckProblem { failures, error: error.to_owned() };
        let (need, why) = problem.text(alondra_core::views::Lang::En);
        let option = QuestionOption { id: STUCK_RETRY.into(), label: STUCK_RETRY_LABEL.into(), description: None };
        match self.domain.raise_worded_problem(&id, STUCK_CAUSE, need, why, problem.to_body(), vec![option], Some(STUCK_RETRY)) {
            Ok(_) => Some(id),
            Err(error) => {
                log!("orchestrator: could not tell the user Claude does not start: {error:?}");
                None
            }
        }
    }

    /// A batch whose effects are unknown is the user's call: a Problem card with the ways out,
    /// which `apply_problem_answers` carries out.
    fn raise_batch_problem(&self, run_id: &str, mut problem: BatchProblem, error: &str) {
        let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
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
        let mut runtime = load(&self.home)?;
        if let Some(problem) = runtime.stuck_problem.clone()
            && let Some(interaction) = state.interactions.get(&InteractionId(problem))
            && (interaction.withdrawn || matches!(&interaction.payload, InteractionPayload::Question { answer: Some(_), .. }))
        {
            runtime.stuck_problem = None;
            if !interaction.withdrawn {
                runtime.retry_at = None;
            }
            save(&self.home, &runtime)?;
        }
        let active = runtime.active_run.map(|run| run.id);
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

/// The durable domain as the orchestrator needs it to act on a batch, which starts a new
/// conversation: every open Request, every question it asked that waits for its answer, and
/// every Task not archived travel in the prompt.
fn summary(state: &DomainState, choices: impl Fn(&alondra_core::domain::RequestId) -> Option<String>) -> String {
    let mut text = String::from("\nDURABLE DOMAIN (open Requests and Tasks not archived):\n");
    let one_line = |body: &str| body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(SUMMARY_LINE).collect::<String>();
    for request in state.requests.values().filter(|request| !request.closed) {
        let unsplit = !state.tasks.values().any(|task| task.request_id == request.id);
        let body = if unsplit { request.body.trim().to_owned() } else { one_line(&request.body) };
        text.push_str(&format!("request {}: {body}\n", request.id));
        let schedule = state.schedule.runs.values().find(|run| run.request_id.as_ref() == Some(&request.id))
            .and_then(|run| state.schedule.schedules.get(&run.schedule_id));
        if let Some(schedule) = schedule {
            text.push_str(&format!("  made by the schedule “{}”: its options below are the plan of every task you create for it\n", one_line(&schedule.title)));
        }
        if let Some(chosen) = choices(&request.id) {
            text.push_str(&chosen);
            text.push('\n');
        }
    }
    for card in state.interactions.values().filter(|card| matches!(card.author, Author::Orchestrator { .. }) && !card.withdrawn) {
        let open = matches!(&card.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. });
        if !open {
            continue;
        }
        let about = match &card.scope {
            Scope::Orchestrator { request_id: Some(request) } => format!(" about request {request}"),
            Scope::Task { task_id } => format!(" about task {task_id}"),
            _ => String::new(),
        };
        text.push_str(&format!("your question {}{about}, still unanswered: {}\n", card.id, one_line(&card.body)));
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

/// At most `limit` bytes of `note`, cut at a character.
fn bounded_note(note: &str, limit: usize) -> String {
    let note = note.trim();
    let end = note.char_indices().map(|(at, c)| at + c.len_utf8()).take_while(|end| *end <= limit).last().unwrap_or(0);
    note[..end].to_owned()
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
    Executor { domain, home: home.to_owned(), claude_home: crate::watcher::claude_home() }
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
    let executor = Executor { domain, home: home.clone(), claude_home };
    let directory = home.join("runtime/orchestrator");
    let _ownership = loop {
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
        if let Err(error) = executor.tick().await {
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

/// An executor whose batches run through the pinned Node host with the official SDK and the fake
/// orchestrator Claude (`agent-host/orchestrator-fake-claude.py`), which answers each prompt as its
/// marker says. A wrapper around the fake records each start with the capability it got, and,
/// from files beside its capture, puts a marker in front of every prompt (`.marker`, or
/// `.marker-once` for the next start only), holds the first prompt before Claude sees it
/// (`.hold-before`) or adds fields to every result (`.result-extra`). A held turn waits on a FIFO
/// the test writes, never on a clock.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::orchestrator_process::Ownership;
    use crate::test_support::Directory;
    use alondra_core::domain::{CardKind, CardSpec, LaunchPlan};
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    const WRAPPER: &str = r#"#!/usr/bin/python3
import json
import os
import sys

capture = os.environ['FIXTURE_CAPTURE']
with open(capture + '.starts', 'a') as stream:
    stream.write(os.environ.get('ALONDRA_DOMAIN_CAPABILITY', '') + '\n')
marker = None
for name in ('.marker', '.marker-once'):
    if os.path.exists(capture + name):
        with open(capture + name) as stream:
            marker = stream.read().strip()
        if name == '.marker-once':
            os.remove(capture + name)
        break
hold_before = os.path.exists(capture + '.hold-before')
extra = json.load(open(capture + '.result-extra')) if os.path.exists(capture + '.result-extra') else {}


class Inputs:
    def __iter__(self):
        for raw in sys.__stdin__:
            message = json.loads(raw)
            if message.get('type') == 'user':
                if hold_before:
                    with open(os.environ['FIXTURE_HELD'], 'w') as held:
                        held.write('held\n')
                    with open(os.environ['FIXTURE_RELEASE']) as release:
                        release.read(1)
                if marker:
                    message['message']['content'] = marker + ' ' + message['message']['content']
            yield json.dumps(message) + '\n'


class Outputs:
    def write(self, text):
        for line in text.splitlines(True):
            if extra and line.strip():
                value = json.loads(line)
                if value.get('type') == 'result':
                    value.update(extra)
                    line = json.dumps(value, ensure_ascii=False) + '\n'
            sys.__stdout__.write(line)

    def flush(self):
        sys.__stdout__.flush()


sys.stdin = Inputs()
sys.stdout = Outputs()
fake = FAKE
sys.argv = [fake] + sys.argv[1:]
exec(compile(open(fake).read(), fake, 'exec'), {'__name__': '__main__'})
"#;

    fn runfile(name: &str) -> PathBuf {
        std::fs::canonicalize(std::env::var(name).unwrap_or_else(|_| panic!("{name} names a runfile"))).unwrap()
    }

    fn fifo(path: &Path) {
        assert!(std::process::Command::new("/usr/bin/mkfifo").arg(path).status().unwrap().success());
    }

    /// Alondra's node and orchestrator plugins, registered in the returned Claude home.
    pub(crate) fn registered_plugins(root: &Path) -> PathBuf {
        let claude_home = root.join("claude-home");
        let repo = root.join("assets");
        for plugin in [crate::plugins::NODE, crate::plugins::ORCHESTRATOR] {
            std::fs::create_dir_all(repo.join(plugin).join(".claude-plugin")).unwrap();
            std::fs::write(repo.join(plugin).join(".claude-plugin/plugin.json"), "{}").unwrap();
        }
        std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
        std::fs::write(claude_home.join("plugins/known_marketplaces.json"), serde_json::json!({"alondra":{"installLocation":repo}}).to_string()).unwrap();
        claude_home
    }

    pub(crate) struct Fixture {
        pub(crate) executor: Executor,
        _ownership: Ownership,
        pub(crate) capture: PathBuf,
        pub(crate) held: PathBuf,
        pub(crate) release: PathBuf,
        pub(crate) claude_home: PathBuf,
        _state: crate::sdk_owner::tests::HostRoot,
        pub(crate) root: Directory,
    }

    /// A daemon home of its own whose orchestrator runs through the SDK.
    pub(crate) fn fixture() -> Fixture {
        let root = Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        Fixture::over(root, domain, home)
    }

    impl Fixture {
        /// The orchestrator of `domain`, whose daemon home is `home`, through the SDK; its fakes live in `root`.
        pub(crate) fn over(root: Directory, domain: Arc<DomainRuntime>, home: PathBuf) -> Self {
            let claude_home = registered_plugins(&root);
            let fake = root.join("orchestrator-fake-claude.py");
            std::fs::copy(runfile("FIXTURE_ORCHESTRATOR_NATIVE"), &fake).unwrap();
            let native = root.join("native");
            std::fs::write(&native, WRAPPER.replace("FAKE", &format!("{:?}", fake.to_str().unwrap()))).unwrap();
            std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
            let (capture, held, release) = (root.join("capture.json"), root.join("held"), root.join("release"));
            fifo(&held);
            fifo(&release);
            let environment = BTreeMap::from([("HOME".to_owned(), root.to_str().unwrap().to_owned()), ("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
            let mut native_environment = environment.clone();
            for (name, path) in [("FIXTURE_CAPTURE", &capture), ("FIXTURE_HELD", &held), ("FIXTURE_RELEASE", &release)] {
                native_environment.insert(name.into(), path.to_str().unwrap().into());
            }
            let (node, host) = (runfile("FIXTURE_NODE"), runfile("FIXTURE_HOST"));
            let state = crate::sdk_owner::tests::HostRoot::new();
            let backend = crate::sdk_owner::SdkBackend {
                node_sha256: crate::sdk_owner::backend::digest(&node).unwrap(),
                host_sha256: crate::sdk_owner::backend::digest(&host).unwrap(),
                native_sha256: crate::sdk_owner::backend::digest(&native).unwrap(),
                node,
                host,
                native,
                host_environment: environment,
                native_environment,
                confinement: None,
                state_root: state.join("h"),
                io_timeout_seconds: 30,
                installed_native: false,
            };
            domain.enable_sdk(backend);
            let ownership = Ownership::acquire(&home.join("runtime/orchestrator")).unwrap().unwrap();
            Self { executor: Executor { domain, home, claude_home: claude_home.clone() }, _ownership: ownership, capture, held, release, claude_home, _state: state, root }
        }

        pub(crate) fn domain(&self) -> &DomainRuntime {
            &self.executor.domain
        }

        pub(crate) fn run(&self, actor: Actor, id: &str, command: Command) {
            let state = self.domain().state().unwrap();
            self.domain().execute(&actor, &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
                command_id: id.into(), expected_revision: state.revision, command,
            }, None, || Ok(())).unwrap();
        }

        /// A node working on task `t` asks the orchestrator `body`: one orchestrator inbox message.
        pub(crate) fn ask(&self, n: u32, body: &str) {
            if self.domain().state().unwrap().tasks.is_empty() {
                self.run(Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
                self.run(Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
                self.run(Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
                    task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
                    review_prs: false, dependencies: BTreeSet::new(), stack: None,
                    launch: Some(LaunchPlan::new("node", "/tmp")),
                });
            }
            self.run(Actor::Agent { agent_id: "a".into() }, &format!("question-{n}"), Command::AskQuestion {
                attachments: vec![], interaction_id: format!("q{n}").into(), scope: Scope::Task { task_id: "t".into() },
                recipient: Recipient::Orchestrator, body: body.into(), blocking: false,
                options: vec![QuestionOption { id: "here".into(), label: "Here".into(), description: None }],
                card: CardSpec::derived(CardKind::Provide, "Answer the question"),
            });
        }

        /// A message from the user: once the orchestrator handled it, `settle` closes it.
        pub(crate) fn tell(&self, id: &str, body: &str) {
            self.run(Actor::User { user_id: "u".into() }, id, Command::CreateRequest { request_id: id.into(), body: body.into(), auto_merge: false, maintenance: false, message: true });
        }

        fn lines(&self, suffix: &str) -> Vec<String> {
            match std::fs::read_to_string(format!("{}{suffix}", self.capture.display())) {
                Ok(text) => text.lines().map(str::to_owned).collect(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
                Err(error) => panic!("{error}"),
            }
        }

        /// Every prompt the fake Claude got, in order.
        pub(crate) fn prompts(&self) -> Vec<String> {
            self.lines(".prompts").iter().map(|line| serde_json::from_str(line).unwrap()).collect()
        }

        /// The capability each start of the fake Claude got, one per start.
        pub(crate) fn starts(&self) -> Vec<String> {
            self.lines(".starts")
        }

        /// What the last start of the fake Claude was launched with.
        pub(crate) fn arguments(&self) -> Vec<String> {
            let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&self.capture).unwrap()).unwrap();
            serde_json::from_value(capture["args"].clone()).unwrap()
        }

        /// Every prompt from now on starts with `marker` until `clear_marker`.
        pub(crate) fn mark(&self, marker: &str) {
            std::fs::write(format!("{}.marker", self.capture.display()), marker).unwrap();
        }

        /// Only the next start's prompts start with `marker`.
        pub(crate) fn mark_once(&self, marker: &str) {
            std::fs::write(format!("{}.marker-once", self.capture.display()), marker).unwrap();
        }

        pub(crate) fn clear_marker(&self) {
            std::fs::remove_file(format!("{}.marker", self.capture.display())).unwrap();
        }

        /// The next starts hold their first prompt before Claude sees it, until `release`.
        pub(crate) fn hold_before_claude(&self) {
            std::fs::write(format!("{}.hold-before", self.capture.display()), "").unwrap();
        }

        /// Every result the fake Claude reports carries `fields` too.
        pub(crate) fn add_to_results(&self, fields: serde_json::Value) {
            std::fs::write(format!("{}.result-extra", self.capture.display()), fields.to_string()).unwrap();
        }

        pub(crate) fn pending(&self) -> usize {
            self.domain().store.lock().unwrap().pending_messages().unwrap().into_iter().filter(|m| m.recipient_kind == "orchestrator").count()
        }

        pub(crate) fn attempts(&self) -> Vec<String> {
            self.domain().store.lock().unwrap().delivery_evidence_page(0).unwrap().into_iter()
                .filter(|i| i.recipient_kind == "orchestrator").map(|i| i.state).collect()
        }

        pub(crate) fn cards(&self, prefix: &str) -> Vec<String> {
            self.domain().state().unwrap().interactions.keys().filter(|id| id.0.starts_with(prefix)).map(|id| id.0.clone()).collect()
        }

        pub(crate) fn unresolved(&self) -> Vec<String> {
            let state = self.domain().state().unwrap();
            let requests: Vec<RequestId> = state.requests.keys().cloned().collect();
            let suggestions: Vec<InteractionId> = state.interactions.keys().cloned().collect();
            crate::orchestrator_inbox::unresolved(&state, &requests, &suggestions)
        }

        pub(crate) async fn tick(&self) {
            self.executor.tick().await.unwrap();
        }

        /// Blocks until the fake Claude holds its turn.
        pub(crate) fn wait_held(&self) {
            let mut text = String::new();
            std::fs::File::open(&self.held).unwrap().read_to_string(&mut text).unwrap();
            assert_eq!(text, "held\n");
        }

        /// Waits, off the runtime, until the fake Claude holds its turn.
        pub(crate) async fn held(&self) {
            let held = self.held.clone();
            tokio::task::spawn_blocking(move || {
                let mut text = String::new();
                std::fs::File::open(held).unwrap().read_to_string(&mut text).unwrap();
            }).await.unwrap();
        }

        pub(crate) fn release(&self) {
            std::fs::OpenOptions::new().write(true).open(&self.release).unwrap().write_all(b"go").unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Fixture, fixture};
    use super::*;
    use alondra_core::domain::*;

    fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// A question from an Agent to the orchestrator: one orchestrator inbox message.
    fn ask_orchestrator(f: &Fixture, n: u32) {
        f.ask(n, &format!("Where do I put change {n}?"));
    }

    fn outbox(domain: &DomainRuntime) -> Vec<(i64, String)> {
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|m| m.recipient_kind == "orchestrator").map(|m| (m.id, m.state)).collect()
    }

    /// The reminders `settle` sent back to the orchestrator when a batch left something open.
    fn reminders(domain: &DomainRuntime) -> Vec<String> {
        use alondra_core::orchestrator_cards::{UNANSWERED_NOTICE_PREFIX, UNSETTLED_NOTICE_PREFIX, UNSPLIT_NOTICE_PREFIX};
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|m| m.recipient_kind == "orchestrator")
            .filter_map(|m| m.interaction_id)
            .filter(|id| [UNANSWERED_NOTICE_PREFIX, UNSETTLED_NOTICE_PREFIX, UNSPLIT_NOTICE_PREFIX].iter().any(|prefix| id.starts_with(prefix)))
            .collect()
    }

    /// The orchestrator's inbox without those reminders.
    fn unhandled(domain: &DomainRuntime) -> Vec<(i64, String)> {
        let reminders = reminders(domain);
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|m| m.recipient_kind == "orchestrator" && m.interaction_id.as_ref().is_none_or(|id| !reminders.contains(id)))
            .map(|m| (m.id, m.state)).collect()
    }

    fn attempts(domain: &DomainRuntime) -> Vec<(i64, String, Option<String>)> {
        domain.store.lock().unwrap().delivery_evidence_page(0).unwrap().into_iter()
            .filter(|i| i.recipient_kind == "orchestrator").map(|i| (i.message_id, i.state, i.reason)).collect()
    }

    fn last_prompt(f: &Fixture) -> String {
        f.prompts().pop().expect("a batch reached Claude")
    }

    fn again(f: &Fixture) {
        let mut runtime = load(&f.executor.home).unwrap();
        runtime.retry_at = None;
        save(&f.executor.home, &runtime).unwrap();
    }

    #[tokio::test]
    async fn a_batch_reaches_one_invocation_with_a_bound_capability_and_is_handed_once() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        ask_orchestrator(&f, 2);
        assert_eq!(outbox(domain).len(), 2);
        f.tick().await;
        assert_eq!(f.starts().len(), 1);
        let prompt = last_prompt(&f);
        assert!(!prompt.contains("data_epoch=") && prompt.contains("Where do I put change 1?")
            && prompt.contains("Where do I put change 2?") && !prompt.contains("inbox message")
            && !prompt.contains("interaction_id=") && prompt.contains("new conversation"), "{prompt}");
        assert!(prompt.contains("request r: Work\n") && prompt.contains("task t of request r: \"Task\" state=not_started agent=a\n"), "{prompt}");
        assert!(unhandled(domain).is_empty());
        assert_eq!(reminders(domain).len(), 2, "the two questions it left unanswered go back once, in the same step");
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
        let token = f.starts()[0].clone();
        assert_eq!(token.len(), 64);
        assert!(domain.resolve_bearer(&token).is_err(), "the execution's bearer is revoked when it ends");
        for n in [1, 2] {
            command(domain, Actor::Orchestrator { generation: 0 }, &format!("answer-{n}"), Command::AnswerQuestion {
                attachments: vec![], interaction_id: format!("q{n}").into(), option_id: "here".into(), inputs: Default::default(),
            });
        }
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "the reminders are a batch of their own");
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "nothing pending, no launch");
        ask_orchestrator(&f, 3);
        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains("new conversation") && prompt.contains("Where do I put change 3?")
            && !prompt.contains("Where do I put change 1?") && !prompt.contains("inbox message"), "{prompt}");
        assert_ne!(load(&f.executor.home).unwrap().session_id, runtime.session_id, "every batch is a conversation of its own");
    }

    /// Recorded from the 2026-10-03 run e8ab8f90: Read on the projects root was refused, the
    /// orchestrator went on to create and launch its task, and Claude reported success.
    #[tokio::test]
    async fn a_run_that_worked_around_a_refused_read_is_handed_and_frees_the_orchestrator() {
        let f = fixture();
        let domain = &f.executor.domain;
        let projects = f.root.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(alondra_core::options::path_in(&f.executor.home), format!("projects_root = {:?}\n", projects.to_str().unwrap())).unwrap();
        f.add_to_results(serde_json::json!({"permission_denials": [{"tool_name": "Read", "tool_use_id": "toolu_01RqEHPzQNkeM7Upm8Mc7Yqu",
            "tool_input": {"file_path": "/Users/example/Projects"}}]}));
        ask_orchestrator(&f, 1);
        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains(&format!("Projects root, readable with Read, Glob and Grep: {}\n", projects.display())), "{prompt}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none(), "{runtime:?}");
        assert_eq!(runtime.last_error, None);
        assert_eq!(runtime.consecutive_failures, 0);
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "handed"));
        ask_orchestrator(&f, 2);
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "the next batch runs at once");
    }

    #[tokio::test]
    async fn a_run_that_cannot_be_prepared_is_released_for_retry_instead_of_left_running() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        let blocked = f.executor.home.join("domain-capabilities.json.next");
        std::fs::create_dir_all(blocked.join("occupied")).unwrap();
        f.tick().await;
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none(), "{runtime:?}");
        assert!(runtime.last_error.as_deref().is_some_and(|error| error.contains("capability")), "{runtime:?}");
        assert!(runtime.retry_at.is_some());
        assert!(f.starts().is_empty(), "Claude is not started");
        assert!(attempts(domain).iter().all(|(_, state, reason)| state == "retryable" && reason.as_deref() == Some("orchestrator_launch_failed")));
        assert_eq!(status(&f.executor).unwrap()["phase"], "failed");
        assert_eq!(status(&f.executor).unwrap()["failure"], "launch");

        std::fs::remove_dir_all(&blocked).unwrap();
        again(&f);
        f.tick().await;
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none(), "{runtime:?}");
        assert!(unhandled(domain).is_empty());
    }

    #[tokio::test]
    async fn stale_orchestrator_messages_expire_before_execution_but_boundary_messages_remain() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        ask_orchestrator(&f, 2);
        command(domain, Actor::System, "user-info", Command::PublishInformation {
            interaction_id: "info-user".into(), scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::User, body: "User notification".into(), attachments: vec![], card: CardSpec::derived(CardKind::Know, "Read the update"),
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

        f.tick().await;

        let store = domain.store.lock().unwrap();
        assert_eq!(store.message_outbox_state_for_test(1).unwrap(), "discarded");
        assert_eq!(store.message_outbox_state_for_test(2).unwrap(), "handed");
        assert_eq!(store.pending_messages().unwrap().iter().filter(|message| message.recipient_kind == "user").count(), 1,
            "user notifications retain their separate delivery policy");
        assert!(store.pending_messages().unwrap().iter().all(|message| message.id != 1));
        drop(store);
        assert_eq!(f.starts().len(), 1, "the fresh message remains eligible for execution");
    }

    #[test]
    fn orchestrator_claim_rechecks_expiry_and_keeps_the_exact_cutoff_eligible() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        ask_orchestrator(&f, 2);
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
        domain.execute(&Actor::User { user_id: "device:phone".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("answer-{option}").into(),
            expected_revision: state.revision,
            command: Command::AnswerQuestion {
                attachments: vec![], interaction_id: format!("{PROBLEM_PREFIX}{run_id}").into(), option_id: option.into(),
                inputs: Default::default(),
            },
        }, None, || Ok(())).unwrap();
    }

    /// Claude created a task and then died: the batch changed something.
    const CREATED_THEN_EXIT: &str = "fixture:tool-then-exit";
    /// Claude died before it answered: the batch changed nothing.
    const DIED_BEFORE_ANSWERING: &str = "fixture:exit-before-init";

    #[tokio::test]
    async fn a_failed_batch_is_a_problem_card_and_its_answer_is_the_decision() {
        for (option, replays) in [("retry", true), ("continue", false)] {
            let f = fixture();
            f.mark(CREATED_THEN_EXIT);
            let domain = &f.executor.domain;
            ask_orchestrator(&f, 1);
            f.tick().await;
            let run = load(&f.executor.home).unwrap().active_run.expect("the failed batch stays claimed");
            let id = format!("{PROBLEM_PREFIX}{}", run.id);
            let problem = domain.state().unwrap().interactions[&id.clone().into()].clone();
            assert_eq!(problem.card.kind, CardKind::Problem);
            assert_eq!(problem.author, Author::System);
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
    async fn a_run_that_changed_nothing_runs_the_batch_again_with_its_prompt_and_asks_nobody() {
        let f = fixture();
        f.mark(DIED_BEFORE_ANSWERING);
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        f.tick().await;
        assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(PROBLEM_PREFIX)),
            "a run that changed nothing is not a decision for the user");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.retry_at.is_some(), "{runtime:?}");
        assert_eq!(runtime.failure, Some(Failure::Launch));
        assert!(runtime.last_error.is_some(), "{runtime:?}");
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "retryable"));

        f.clear_marker();
        again(&f);
        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains("Where do I put change 1?") && !prompt.contains(DIED_BEFORE_ANSWERING), "{prompt}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none(), "{runtime:?}");
        assert!(unhandled(domain).is_empty());
    }

    #[tokio::test]
    async fn claude_failing_to_start_three_times_in_a_row_tells_the_user_once_and_a_good_run_takes_it_back() {
        let f = fixture();
        f.mark(DIED_BEFORE_ANSWERING);
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        let stuck = || domain.state().unwrap().interactions.values()
            .filter(|i| i.id.0.starts_with(STUCK_PROBLEM_PREFIX) && !i.withdrawn).map(|i| i.id.clone()).collect::<Vec<_>>();
        for failure in 1..=4 {
            f.tick().await;
            assert_eq!(stuck().len(), usize::from(failure >= STUCK_AFTER), "after {failure} failed starts");
            again(&f);
        }
        let problem = stuck().remove(0);
        let state = domain.state().unwrap();
        let card = &state.interactions[&problem];
        assert_eq!(card.card.kind, CardKind::Problem);
        assert_eq!(card.card.recommended.as_deref(), Some(STUCK_RETRY));
        assert!(StuckProblem::from_body(&card.body).is_some_and(|body| body.failures == STUCK_AFTER), "{}", card.body);

        f.clear_marker();
        f.tick().await;
        assert!(stuck().is_empty(), "the orchestrator works again: nothing is left for the user");
        assert!(load(&f.executor.home).unwrap().stuck_problem.is_none());
    }

    #[tokio::test]
    async fn trying_again_now_from_the_card_starts_the_orchestrator_without_waiting_for_its_backoff() {
        let f = fixture();
        f.mark(DIED_BEFORE_ANSWERING);
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        for _ in 0..STUCK_AFTER {
            again(&f);
            f.tick().await;
        }
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.retry_at.is_some(), "{runtime:?}");
        let problem = runtime.stuck_problem.clone().expect("the user was told");
        let state = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "u".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "retry-now".into(),
            expected_revision: state.revision,
            command: Command::AnswerQuestion { attachments: vec![], interaction_id: InteractionId(problem), option_id: STUCK_RETRY.into(), inputs: Default::default() },
        }, None, || Ok(())).unwrap();
        f.executor.apply_problem_answers().unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.retry_at.is_none() && runtime.stuck_problem.is_none(), "{runtime:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_stopped_before_it_changed_anything_is_settled_without_a_card() {
        let f = fixture();
        f.hold_before_claude();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        let canceller = async {
            f.held().await;
            let run = load(&f.executor.home).unwrap().active_run.unwrap().id;
            f.executor.cancel(&run).unwrap();
        };
        let ((), ()) = tokio::join!(f.tick(), canceller);
        assert!(f.prompts().is_empty(), "Claude never saw the batch");
        assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(PROBLEM_PREFIX)));
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && !runtime.cancel_requested, "{runtime:?}");
        assert_eq!(runtime.pending_resolutions.len(), 1, "the orchestrator hears it was stopped");
        assert!(outbox(domain).is_empty(), "a stopped batch is not replayed");
    }

    #[tokio::test]
    async fn a_batch_settled_elsewhere_withdraws_its_problem() {
        let f = fixture();
        f.mark(CREATED_THEN_EXIT);
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        f.tick().await;
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        f.executor.acknowledge(&run.id).unwrap();
        f.executor.apply_problem_answers().unwrap();
        assert!(domain.state().unwrap().interactions[&format!("{PROBLEM_PREFIX}{}", run.id).into()].withdrawn);
    }

    #[tokio::test]
    async fn a_failed_execution_is_ambiguous_until_acknowledged_and_resolved_at_restart() {
        let f = fixture();
        f.mark(CREATED_THEN_EXIT);
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        f.tick().await;
        let runtime = load(&f.executor.home).unwrap();
        let run = runtime.active_run.clone().expect("an ambiguous execution stays claimed");
        assert!(runtime.last_error.is_some());
        assert_eq!(runtime.failure, Some(Failure::Interrupted));
        assert!(attempts(domain).iter().all(|(_, state, reason)| state == "uncertain" && reason.as_deref() == Some("orchestrator_execution_ambiguous")));
        assert!(outbox(domain).is_empty(), "an ambiguous batch is not offered again");
        f.tick().await;
        assert_eq!(f.starts().len(), 1);
        assert!(f.executor.acknowledge("other-run").is_err());
        f.executor.acknowledge(&run.id).unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && runtime.failure.is_none());
        assert_eq!(runtime.pending_resolutions, vec![Resolution { run_id: run.id.clone(), message_ids: run.message_ids.clone() }]);
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "discarded"));

        f.clear_marker();
        ask_orchestrator(&f, 2);
        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains("Previous interrupted inbox batches have already been resolved")
            && !prompt.contains(&run.id) && !prompt.contains("inbox message IDs"), "{prompt}");
        assert!(prompt.contains("Where do I put change 2?") && !prompt.contains("Where do I put change 1?")
            && !prompt.contains("inbox message"), "{prompt}");
        assert!(load(&f.executor.home).unwrap().pending_resolutions.is_empty());

        // A daemon restart during an execution resolves it without the user.
        let mut runtime = load(&f.executor.home).unwrap();
        ask_orchestrator(&f, 3);
        let pending = unhandled(domain)[0].0;
        let claim = domain.store.lock().unwrap().claim_message(pending).unwrap().unwrap();
        runtime.active_run = Some(Run { id: "interrupted".into(), message_ids: vec![claim.message.id],
            attempts: vec![(claim.attempt_id, claim.version)], started_at: 1, spawned: true });
        save(&f.executor.home, &runtime).unwrap();
        let recovered = f.executor.recover().unwrap();
        assert!(recovered.active_run.is_none());
        assert_eq!(recovered.pending_resolutions[0].run_id, "interrupted");
        assert!(unhandled(domain).is_empty(), "the interrupted batch is not replayed");
        assert!(attempts(domain).iter().any(|(id, state, _)| *id == claim.message.id && state == "discarded"));
    }

    #[tokio::test]
    async fn an_interrupted_batch_is_not_replayed_but_one_fresh_run_sees_its_open_request() {
        let f = fixture();
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

        f.tick().await;
        assert_eq!(f.starts().len(), 1);
        let prompt = last_prompt(&f);
        assert!(prompt.contains("Previous interrupted inbox batches have already been resolved")
            && prompt.contains("An open Request below with no Task of its own was not turned into work")
            && prompt.contains("request bc2466f2: Make the editor readable\n")
            && !prompt.contains("The user created Request"), "{prompt}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.pending_resolutions.is_empty() && runtime.active_run.is_none() && runtime.last_error.is_none());

        assert_eq!(reminders(domain), ["request-unsplit-bc2466f2-1"], "the request it left without a task goes back once, in the same step");
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "the reminder is a batch of its own");
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "then it is the user's question: one fresh run, not a loop");
    }

    /// 21 October: David accepted a design whose plan has six phases and asked for them before the
    /// orchestrator's batch, already creating them, had finished.
    #[tokio::test]
    async fn an_accepted_plan_in_phases_reaches_the_orchestrator_at_once_and_one_left_without_tasks_is_flagged() {
        use alondra_core::orchestrator_cards::{UNSPLIT_NOTICE_PREFIX, UNSPLIT_PROBLEM_PREFIX};
        let f = fixture();
        let domain = &f.executor.domain;
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        command(domain, Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest {
            request_id: "r".into(), body: "Design several Claude accounts".into(), auto_merge: false, maintenance: false, message: false,
        });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
            task_id: "design".into(), request_id: "r".into(), title: "Design several Claude accounts".into(), agent_id: Some("a".into()),
            review_prs: false, dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
        });
        let plan = "# Several accounts\n\n## Phase 1\n\n- [ ] Accounts in options\n\n## Phase 2\n\n- [ ] Usage per account\n";
        let targets = vec![DeliveryTarget::Text { text: "Design".into() }];
        command(domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
            checklist: vec![], attachments: vec![], task_id: "design".into(), interaction_id: "design-result".into(),
            body: "Read the design".into(), card: CardSpec::delivery("Read the design", &targets, true).plan(plan),
            targets: targets.clone(), is_final: true, nothing_to_review: false,
        });
        command(domain, Actor::User { user_id: "u".into() }, "implement", Command::DecideProposedPlan {
            presentation_id: "design-result".into(), targets, choice: PlanChoice::Implement,
        });
        assert_eq!(outbox(domain).len(), 1, "the acceptance alone puts the plan in the orchestrator's inbox");

        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains("Request implement-design-result holds that plan") && prompt.contains("one per phase"), "{prompt}");

        let reminder = format!("{UNSPLIT_NOTICE_PREFIX}implement-design-result-1");
        assert_eq!(reminders(domain), [reminder.clone()], "a batch that left the plan without tasks is told so");
        let text = domain.state().unwrap().interactions[&InteractionId::from(reminder.as_str())].body.clone();
        assert!(text.contains("holds a plan the user approved and still has no implementation Task")
            && text.contains("one per phase") && !text.contains("answer it and close it"), "{text}");

        f.tick().await;
        assert_eq!(f.cards(UNSPLIT_PROBLEM_PREFIX), [format!("{UNSPLIT_PROBLEM_PREFIX}implement-design-result-1")],
            "left without tasks again, it is the user's problem");
    }

    #[test]
    fn each_open_request_carries_the_choices_it_was_asked_with() {
        let f = fixture();
        ask_orchestrator(&f, 1);
        let state = f.executor.domain.state().unwrap();
        let text = summary(&state, |request| (request.0 == "r").then(|| "  options: project App · Effort: hard".to_owned()));
        assert!(text.contains("request r: Work\n  options: project App · Effort: hard\ntask t"), "{text}");
    }

    #[test]
    fn a_request_not_split_yet_reaches_the_orchestrator_whole() {
        let f = fixture();
        let long = format!("{} end of the message", "word ".repeat(80));
        command(&f.executor.domain, Actor::User { user_id: "u".into() }, "long", Command::CreateRequest {
            request_id: "long".into(), body: long.clone(), auto_merge: false, maintenance: false, message: false,
        });
        let text = summary(&f.executor.domain.state().unwrap(), |_| None);
        assert!(text.contains("end of the message"), "{text}");
    }

    #[tokio::test]
    async fn the_user_decides_on_an_ambiguous_execution_through_its_status() {
        let f = fixture();
        f.mark(CREATED_THEN_EXIT);
        let domain = &f.executor.domain;
        assert_eq!(status(&f.executor).unwrap()["phase"], "idle");
        ask_orchestrator(&f, 1);
        assert_eq!(status(&f.executor).unwrap()["queue_depth"], 1);
        f.tick().await;
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
        assert!(domain.state().unwrap().interactions.keys().any(|id| id.0 == format!("{PROBLEM_PREFIX}{}", run.id)));
    }

    #[tokio::test]
    async fn a_retry_offers_the_batch_again_and_a_cancel_is_ambiguous() {
        let f = fixture();
        f.mark(CREATED_THEN_EXIT);
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        f.tick().await;
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        f.executor.retry(&run.id).unwrap();
        assert_eq!(outbox(domain).len(), 1);
        assert_eq!(outbox(domain)[0].1, "retryable");
        assert!(load(&f.executor.home).unwrap().active_run.is_none());
        assert!(f.executor.cancel("nothing").is_err(), "cancel needs a running execution");
    }

    #[tokio::test]
    async fn no_batch_starts_while_usage_is_exhausted_and_it_runs_once_when_usage_is_back() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        domain.live.usage_pause.exhaust(Some(now_millis() + 3_600_000), now_millis());
        f.tick().await;
        f.tick().await;
        assert_eq!(f.starts().len(), 0, "paused: Claude is not started");
        assert_eq!(outbox(domain).len(), 1, "and the batch waits, untouched");
        assert!(load(&f.executor.home).unwrap().active_run.is_none());

        domain.live.usage_pause.resume(now_millis());
        f.tick().await;
        assert_eq!(f.starts().len(), 1, "resumed: the pending batch runs");
        assert!(unhandled(domain).is_empty());
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "and only once: the next run is the reminder of the question it left open");
    }

    #[tokio::test]
    async fn a_run_that_hits_the_limit_before_working_pauses_everything_and_is_retried_after_the_reset() {
        let f = fixture();
        f.mark_once("fixture:limited");
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        let before = now_millis() / 1000;
        f.tick().await;
        let after = now_millis() / 1000;
        assert_eq!(f.starts().len(), 1);
        let until = domain.live.usage_pause.current().map(|e| e.until).expect("usage pauses everything");
        assert!(until % 1000 == 0 && ((before + 3_600) * 1000..=(after + 3_600) * 1000).contains(&until),
            "until is the reset Claude reported, an hour after the run: {until}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && runtime.failure.is_none() && runtime.consecutive_failures == 0 && runtime.retry_at.is_none(), "{runtime:?}");
        assert_eq!(outbox(domain).iter().map(|(_, state)| state.as_str()).collect::<Vec<_>>(), ["retryable"]);
        assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(PROBLEM_PREFIX)), "a limit is not a Problem for the user to decide");

        f.tick().await;
        assert_eq!(f.starts().len(), 1, "nothing runs until the reset");
        let usage = domain.read().state().account_usage.clone();
        assert!(usage.iter().any(|usage| usage.account.is_default() && usage.exhausted_until(now_millis()) == Some(until)),
            "the orchestrator's account ran out until the reset: {usage:?}");

        crate::usage_probe::record(domain, &alondra_core::accounts::AccountId::default_account(), alondra_core::usage::UsageReading {
            windows: vec![alondra_core::usage::WindowReading { kind: alondra_core::usage::WindowKind::Session, used: alondra_core::usage::Share::of(0.1), resets_at: None, exhausted: false }],
            plan: None, complete: true,
        }, now_millis());
        domain.live.usage_pause.resume(now_millis());
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "after the reset the probe finds room: the same batch is retried");
        assert!(unhandled(domain).is_empty());
        assert!(attempts(domain).iter().any(|(_, state, _)| state == "handed"));
    }

    #[tokio::test]
    async fn without_an_sdk_backend_a_batch_does_not_start_and_is_offered_again() {
        let root = crate::test_support::Directory::new();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let claude_home = super::testing::registered_plugins(&root);
        let _ownership = crate::orchestrator_process::Ownership::acquire(&home.join("runtime/orchestrator")).unwrap().unwrap();
        let executor = Executor { domain: domain.clone(), home: home.clone(), claude_home };
        command(&domain, Actor::User { user_id: "u".into() }, "m-1", Command::CreateRequest {
            request_id: "m-1".into(), body: "Where do I put change 1?".into(), auto_merge: false, maintenance: false, message: true,
        });
        executor.tick().await.unwrap();
        let runtime = load(&home).unwrap();
        assert_eq!(runtime.failure, Some(Failure::Launch), "{runtime:?}");
        assert_eq!(runtime.last_error.as_deref(), Some(crate::sdk_owner::NO_HOST));
        assert!(runtime.active_run.is_none() && runtime.retry_at.is_some(), "{runtime:?}");
        assert!(attempts(&domain).iter().all(|(_, state, _)| state == "retryable"));
    }
}
